# ArceOS 教程练习实验报告

## 实验总览

五个练习从易到难，覆盖了 OS 开发的核心链路：

```
彩色输出 → 集合类型支持 → 内存分配器 → 文件系统 → 用户态系统调用
 (ANSI)    (HashMap)     (Bump Alloc)  (Rename)     (mmap)
```

每个练习都是独立的 ArceOS unikernel crate，通过 `cargo xtask run` 在 QEMU 中运行验证。

---

## 练习 1：exercise-printcolor（彩色输出）

### 任务目标

让 ArceOS 在 QEMU 虚拟串口上输出**带颜色**的文字。在 bare-metal 的 no_std 环境下，不能使用 `colored` 等 Rust crate，但可以直接用 ANSI 转义码——QEMU 的虚拟串口（uart8250）会将这些字节原样传给终端，终端负责解析颜色。

### 测试

```bash
cd exercise-printcolor
cargo xtask run --arch riscv64
```

如果实现正确，终端会显示**绿色**的 `[WithColor]: Hello, Arceos!`，而不是默认的白色/灰色文字。

### 具体修改

**文件：`exercise-printcolor/src/main.rs`**（第 9 行）

```rust
// 修改前：
println!("Hello, Arceos!");

// 修改后：
println!("\x1b[32m[WithColor]: Hello, Arceos!\x1b[0m");
```

- `\x1b[32m` — ANSI 转义码，设置前景色为绿色
- `\x1b[0m` — 重置所有样式

只改了**一行**，零依赖，零 bug。

---

## 练习 2：exercise-hashmap（HashMap 支持）

### 任务目标

在 ArceOS 的 `axstd`（标准库替代）中添加 `HashMap` 和 `HashSet` 支持，使得 `use std::collections::HashMap`（其中 `std = axstd`）能编译通过。

Rust 的 `alloc::collections` 不包含 `HashMap`，因为 `HashMap` 的默认 hasher（`RandomState`）需要 OS 级随机数源，而 `alloc` crate 无法提供。解决方案是引入 `hashbrown`——Rust 标准库内部使用的 hash table 实现，支持 no_std。

### 怎么测试

```bash
cd exercise-hashmap
cargo xtask run --arch riscv64
```

程序会创建 `HashMap`，插入键值对，然后遍历打印。如果编译通过并正确输出 map 内容，说明 `HashMap` 集成成功。

### 具体修改

**1. 下载 axstd 源码到本地**

```bash
cd exercise-hashmap && cargo clone axstd@0.3.0-preview.1
```

**2. 文件：`exercise-hashmap/axstd/Cargo.toml`**

添加 `hashbrown` 依赖：

```toml
[dependencies]
hashbrown = { version = "0.15", default-features = false, features = ["raw"] }
```

> `default-features = false` 确保不启用 std feature，否则在 no_std 下编译失败。

**3. 文件：`exercise-hashmap/axstd/src/lib.rs`**（collections 模块）

```rust
// 修改前：pub use alloc::collections;
// 修改后：
pub mod collections {
    pub use alloc::collections::*;
    pub use hashbrown::{HashMap, HashSet};
}
```

将原来直接 re-export 的 `alloc::collections` 展开为自定义模块，先导出 `alloc::collections::*`（Vec, String 等），再补充 `hashbrown` 的 `HashMap` 和 `HashSet`。

**4. 文件：`exercise-hashmap/Cargo.toml`**

```toml
# 修改前：
axstd = { version = "=0.3.0-preview.1", features = ["defplat", "alloc"], optional = true }
# 修改后：
axstd = { path = "./axstd", features = ["defplat", "alloc"], optional = true }
```

将依赖从 crates.io 版本号改为本地路径，使用我们修改后的 axstd。

---

## 练习 3：exercise-altalloc（Bump Allocator）

### 任务目标

实现一个 **Bump Allocator**（也叫指针碰撞分配器），作为 ArceOS 的早期内存分配器。这个分配器需要实现三个 trait：`BaseAllocator`、`ByteAllocator`、`PageAllocator`。

Bump Allocator 的核心思想极其简单：维护一个指针，每次分配时将指针向前移动。释放时不回收单个分配，而是当所有字节分配都被释放后（count 归零），一次性重置整个区域。

### 内存布局设计

```
[  字节已用  |     可用区域     |  页已用  ]
|            |  -->      <--   |          |
start       b_pos            p_pos      end
```

- **字节分配**：从 `start` 向 `end` 方向增长（`b_pos` 前移）
- **页分配**：从 `end` 向 `start` 方向增长（`p_pos` 后退）
- **count**：记录活跃的字节分配数量，归零时重置 `b_pos`

### 怎么测试

```bash
cd exercise-altalloc
cargo xtask run --arch riscv64
```

测试代码会创建一个 300 万元素的 `Vec`，进行排序操作。这验证了分配器能正确处理大量的 alloc/dealloc（sort 产生临时缓冲区，之后 drop 释放，count 归零后 bump 区域重置）。

### 具体修改

**文件：`exercise-altalloc/modules/bump_allocator/src/lib.rs`** — 完整实现

核心数据结构（5 个字段）：

```rust
pub struct EarlyAllocator<const PAGE_SIZE: usize> {
    start: usize,   // 区域起始地址
    end: usize,     // 区域结束地址
    b_pos: usize,   // 字节分配指针（正向增长）
    p_pos: usize,   // 页分配指针（反向增长）
    count: usize,   // 活跃字节分配计数
}
```

关键方法：

- `**ByteAllocator::alloc**`（第 43-53 行）：对齐 `b_pos`，检查不越过 `p_pos`，前移指针，`count += 1`
- `**ByteAllocator::dealloc**`（第 55-59 行）：使用 `count.saturating_sub(1)` 防止下溢，归零时重置 `b_pos = start`
- `**PageAllocator::alloc_pages**`（第 72-80 行）：从 `p_pos` 反向分配，对齐到 `align_pow2`

> ⚠️ `dealloc` 中必须用 `saturating_sub(1)` 而不是 `count -= 1`，因为 release 模式下 usize 溢出不会 panic，会静默回绕到 `usize::MAX`，导致 count 永远无法归零。

---

## 练习 4：exercise-ramfs-rename（RAM 文件系统 Rename）

### 任务目标

为 ArceOS 的 RAM 文件系统（`axfs_ramfs`）实现 `rename` 操作。RAM 文件系统在内存中维护目录树（`BTreeMap<String, VfsNodeRef>`），rename 的本质是将一个节点（`Arc` 引用）从一个目录的 children map 移到另一个。

### 背景

QEMU 启动时使用一个零字节的 disk.img，导致 axfs 找不到可用的磁盘分区，自动将 ramfs 挂载为根文件系统。这个 exercise 的测试代码会在 ramfs 上创建文件，然后调用 `rename` 重命名。

### 怎么测试

```bash
cd exercise-ramfs-rename
cargo xtask run --arch riscv64
```

测试代码流程：创建文件 `/tmp/f1` → 写入内容 → `rename` 为 `/tmp/f2` → 读取验证内容一致。

### 具体修改

**1. 下载源码**

```bash
cd exercise-ramfs-rename
cargo clone axfs@0.3.0-preview.1
cargo clone axfs_ramfs@0.1.2
```

**2. 文件：`exercise-ramfs-rename/axfs_ramfs/src/dir.rs`**

添加 `rename` 方法到 `VfsNodeOps for DirNode` impl（约第 158-208 行）：

```rust
fn rename(&self, src_path: &str, dst_path: &str) -> VfsResult {
    let (src_parent, src_name) = split_last_component(src_path);
    let (dst_parent, dst_name) = split_last_component(dst_path);
    
    let src_dir = self.navigate_to(src_parent)?;
    let dst_dir = self.navigate_to(dst_parent)?;
    
    // 🔑 关键：检测是否为同一目录，避免死锁
    let is_same_dir = core::ptr::eq(
        src_dir.as_ref() as *const dyn VfsNodeOps as *const (),
        dst_dir.as_ref() as *const dyn VfsNodeOps as *const (),
    );
    
    if is_same_dir {
        // 同一目录：单次写锁内完成 remove + insert
        let node = src_dir.children.write().remove(src_name).ok_or(VfsError::NotFound)?;
        src_dir.children.write().insert(dst_name.into(), node);
    } else {
        // 不同目录：先从源目录取走，再插入目标目录
        // ...
    }
}
```

添加辅助方法 `navigate_to`（约第 103-110 行）：

```rust
pub fn navigate_to(&self, path: &str) -> VfsResult<VfsNodeRef> {
    if path.is_empty() || path == "." {
        return self.this.upgrade().ok_or(VfsError::NotFound).map(|n| n as VfsNodeRef);
    }
    self.clone().lookup(path)
}
```

添加辅助函数 `split_last_component`（文件末尾）：

```rust
fn split_last_component(path: &str) -> (&str, &str) {
    // "tmp/f1" -> ("tmp", "f1"), "f1" -> ("", "f1")
    let path = path.trim_end_matches('/');
    match path.rfind('/') {
        Some(pos) => (&path[..pos], &path[pos + 1..]),
        None => ("", path),
    }
}
```

**3. 文件：`exercise-ramfs-rename/Cargo.toml`**

```toml
[patch.crates-io]
axfs = { path = "./axfs" }
axfs_ramfs = { path = "./axfs_ramfs" }
```

> ⚠️ 同目录 rename 的死锁问题：`spin::RwLock` 不支持重入。如果先 `children.write().remove()` 再 `children.write().insert()`，会获取两次写锁导致死锁。解决方法是用 `core::ptr::eq` 检测是否为同一目录，如果是则合并为同一写锁作用域内的操作。

---

## 练习 5：exercise-sysmap（sys_mmap 系统调用）

### 任务目标

在 ArceOS 的用户态系统调用模拟层中实现 `sys_mmap`。整个 exercise 已经实现了 open、close、read、write、brk、exit 等系统调用以及完整的 ELF 加载器和文件描述符表，唯一缺失的就是 `mmap`。

### 背景

内核创建用户地址空间，从文件系统加载 ELF 到 `/sbin/mapfile`，映射用户栈，然后进入用户态循环处理系统调用。测试程序（C 语言）的流程：

```
creat("file") → write("hello, arceos!") → close → open("file") → mmap(fd) → 从映射地址读取 → 打印
```

### 怎么测试

```bash
cd exercise-sysmap
cargo xtask run --arch riscv64
```

如果 `sys_mmap` 实现正确，用户程序会打印出通过 mmap 读取到的 `hello, arceos!`。

### 具体修改

**文件：`exercise-sysmap/src/syscall.rs`**

**1. 添加导入**（文件头部，第 2-8 行）：

```rust
use alloc::sync::Arc;      // 克隆地址空间 Arc 引用
use axio::Seek;             // 文件 seek 操作 trait
```

**2. 添加地址计数器**（约第 361 行）：

```rust
static MMAP_NEXT_ADDR: AtomicUsize = AtomicUsize::new(0x1000_0000);
```

从 `0x1000_0000` 开始单调递增，每次 mmap 分配一段虚拟地址。

**3. 实现 `sys_mmap`**（替换原来的 `unimplemented!`）：

```rust
fn sys_mmap(_addr, length, prot, flags, fd, offset) -> isize {
    // ① 解析 prot/flags 位标志
    let mmap_prot = MmapProt::from_bits(prot)?;
    let mmap_flags = MmapFlags::from_bits(flags)?;
    
    // ② 长度对齐到 4KB
    let aligned_len = (length + 4095) & !4095;
    
    // ③ 从计数器分配虚拟地址
    let vaddr = MMAP_NEXT_ADDR.fetch_add(aligned_len, Ordering::Relaxed);
    
    // ④ 获取用户地址空间，映射物理页
    let mut uspace = USER_ASPACE.lock().as_ref()...lock();
    uspace.map_alloc(vaddr, aligned_len, mapping_flags, true)?;
    
    // ⑤ 如果是文件映射（非匿名），读取文件内容写入映射区
    if !MAP_ANONYMOUS && fd >= 0 {
        let buf = read_file_at(fd, offset, length);
        uspace.write(vaddr, &buf)?;
    }
    
    vaddr as isize  // 返回映射的虚拟地址
}
```

核心流程图：

```
用户调用 mmap(NULL, 32, PROT_READ, MAP_PRIVATE, fd, 0)
         │
         ▼
  ┌─ 分配虚拟地址 (0x1000_0000) ─┐
  │                                │
  ▼                                ▼
  map_alloc()                  读取文件内容
  (分配物理页+建立映射)         (seek + read)
  │                                │
  └────── uspace.write() ◄─────────┘
         │
         ▼
  返回虚拟地址 0x1000_0000
  (用户程序之后直接读该地址即可获得文件内容)
```

---

## 学习效果总结

### 🎯 核心知识点


| 练习           | 核心概念          | 关键技术                                             |
| ------------ | ------------- | ------------------------------------------------ |
| printcolor   | bare-metal 输出 | ANSI 转义码，QEMU 串口                                 |
| hashmap      | Cargo 依赖管理    | `[patch]`, feature unification, hashbrown no_std |
| altalloc     | 内存分配器设计       | Bump allocator, 双端布局, count-based 释放             |
| ramfs-rename | VFS 文件系统      | 目录项操作, BTreeMap, RwLock 死锁                       |
| sysmap       | 用户态系统调用       | 页表映射, 地址空间, mmap 语义                              |


### 💡 最大收获

1. **Cargo 的两种本地覆盖方式**：`path` 依赖（直接替换）vs `[patch.crates-io]`（保持依赖树一致性）
2. **Bump Allocator 的设计哲学**：极简（O(1) 分配）但有局限（无法单独释放），适合 boot 阶段
3. **Rename 是目录操作不是文件操作**：被 rename 的 inode 不变，变化的是目录的 children map
4. **mmap 的最小实现**：分配物理页 → 映射到虚拟地址 → 复制文件内容 → 返回地址
5. **并发安全意识**：`spin::RwLock` 不支持重入，需要用 `core::ptr::eq` 检测同目录避免死锁

### 🤖 AI 协作的体会

**AI 擅长的事**：快速生成 trait 实现代码、跨文件分析依赖关系、从已有代码模式推断接口签名。

**需要人工关注的事**：并发安全（锁重入、死锁）、unsigned integer overflow（`saturating_sub`）、是否遵循练习的设计意图而非绕过。

---

## 编译测试与 Bug 修复报告（最终版）

### 测试环境
- **测试时间**: 2026 年 4 月 19 日
- **宿主机**: macOS Darwin 25.2.0 (aarch64, Apple Silicon)
- **Rust 工具链**: `nightly-2025-12-12-aarch64-apple-darwin`（项目 `rust-toolchain.toml` 指定）
- **QEMU**: `qemu-system-{riscv64,aarch64}` 10.2.1（Homebrew）
- **C 交叉工具链**（仅 sysmap 需要）: `aarch64-unknown-linux-musl` 15.2.0
  （通过 `brew tap messense/macos-cross-toolchains && brew install aarch64-unknown-linux-musl` 获得）

### 测试结果概览

| Exercise | 目标架构 | 编译 | 运行 | 是否有 Bug | 备注 |
|----------|---------|------|------|-----------|------|
| exercise-printcolor    | riscv64 | ✅ | ✅ | 无 | 输出绿色 `[WithColor]: Hello, Arceos!` |
| exercise-hashmap       | riscv64 | ✅ | ✅ | ✅ 已修复 | hashbrown 缺 `default-hasher` feature |
| exercise-altalloc      | riscv64 | ✅ | ✅ | ✅ 已修复 | 用错了错误类型 `LinuxError` → `AllocError` |
| exercise-ramfs-rename  | riscv64 | ✅ | ✅ | ✅ 已修复 | 3 处 bug（见下） |
| exercise-sysmap        | aarch64 | ✅ | ✅ | 无 | 用户态打印 `Read back content: hello, arceos!` |

> 说明：sysmap 用 aarch64 而非 riscv64，因为 macOS 上暂无 `riscv64-linux-musl-gcc`，但 `aarch64-linux-musl-gcc` 可通过 brew 安装；代码本身是架构无关的。

---

### 练习 1：exercise-printcolor —— 无 bug

直接 `cargo xtask run --arch riscv64`，QEMU 输出：

```
[WithColor]: Hello, Arceos!     ← 绿色
Shutting down...
```

---

### 练习 2：exercise-hashmap —— 修复 1 处 bug

**Bug 现象**（编译期错误）：

```
error[E0599]: no function or associated item named `new` found for struct `HashMap`
  --> src/main.rs:19:26
```

**根因**：`axstd/Cargo.toml` 为 hashbrown 声明了：

```toml
hashbrown = { version = "0.15", default-features = false, features = ["inline-more"] }
```

hashbrown 0.15 默认实现 `HashMap::new()` 依赖 `default-hasher` feature（提供 `DefaultHashBuilder`）。
由于 `default-features = false` 且未显式添加 `default-hasher`，导致无 hasher，`HashMap::new()` 不存在。

**修复**：在 `exercise-hashmap/axstd/Cargo.toml` 里追加 `default-hasher`：

```toml
[dependencies.hashbrown]
version = "0.15"
default-features = false
features = ["inline-more", "default-hasher"]
```

**运行结果**：

```
Running memory tests...
test_hashmap() OK!
Memory tests run OK!
```

---

### 练习 3：exercise-altalloc —— 修复 1 处 bug

**Bug 现象**（编译期类型错误）：

```
error[E0308]: mismatched types
  --> modules/bump_allocator/src/lib.rs:64:20
   |
64 |             return Err(axerrno::LinuxError::ENOMEM);
   |                        ^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected `AllocError`, found `LinuxError`
```

**根因**：`ByteAllocator::alloc` / `PageAllocator::alloc_pages` / `alloc_pages_at` 的返回类型是 `AllocResult<T> = Result<T, AllocError>`，但代码误用了 `axerrno::LinuxError::ENOMEM`。

**修复**：在 `exercise-altalloc/modules/bump_allocator/src/lib.rs` 里，导入并替换：

```rust
use axallocator::{AllocError, AllocResult, BaseAllocator, ByteAllocator, PageAllocator};
// ...
return Err(AllocError::NoMemory);   // 原 axerrno::LinuxError::ENOMEM
```

总共 3 处替换（第 64、100、117 行）。

**运行结果**：

```
use bump allocator.
Running bump tests...
Bump tests run OK!
```

---

### 练习 4：exercise-ramfs-rename —— 修复 3 处 bug

这道题有三个层级的 bug，全部需要修复才能跑通。

#### Bug ①：`DirNode::navigate_to` 调用方式错误（编译错误）

```
error[E0599]: no method named `lookup` found for reference `&DirNode` in the current scope
  --> axfs_ramfs/src/dir.rs:66:22
```

**根因**：`VfsNodeOps::lookup` 的签名是 `fn lookup(self: Arc<Self>, path: &str)`，必须以 `Arc<Self>` 作为接收器。原实现 `self.clone().lookup(path)` 因为 `self: &DirNode`，`clone()` 仍是 `&DirNode`，无法调用 trait 方法。

**修复**：通过 `self.this` 这个 `Weak<DirNode>` 升级到 `Arc<DirNode>`，再转为 `VfsNodeRef` 调用：

```rust
pub fn navigate_to(&self, path: &str) -> VfsResult<VfsNodeRef> {
    let this = self.this.upgrade().ok_or(VfsError::NotFound)?;
    if path.is_empty() || path == "." {
        return Ok(this as VfsNodeRef);
    }
    (this as VfsNodeRef).lookup(path)
}
```

#### Bug ②：`src/main.rs` 中 `io::Error` 的用法（编译错误）

```
error[E0599]: no method named `kind` found for struct `AxError` in the current scope
  --> src/main.rs:14:14
   |
14 |         if e.kind() == io::ErrorKind::AlreadyExists {
```

**根因**：axstd 里的 `io::Error` 实际是 `axio::Error = AxError`，它本身就是一个 enum（`AxError::AlreadyExists` 是变体，不存在 `.kind()`/`ErrorKind`）。

**修复**：直接用 `==` 与 `io::Error::AlreadyExists` 比较：

```rust
fn create_dir(path: &str) -> io::Result<()> {
    println!("Create directory '{}' ...", path);
    fs::create_dir(path).or_else(|e| {
        if e == io::Error::AlreadyExists {
            Ok(())
        } else {
            Err(e)
        }
    })
}
```

#### Bug ③：`RootDirectory` 未转发 `rename` 到挂载文件系统（运行时错误）

**现象**：编译通过，运行时：

```
Rename '/tmp/f1' to '/tmp/f2' ...
[AxError::Unsupported]
panicked at src/main.rs:62:9:
Error: Operation not supported
```

**根因**：`axfs::root::rename` 会调用 `parent_node_of(None, old).rename(...)`，而 `parent_node_of` 对绝对路径返回的是 `RootDirectory`，不是 ramfs 的 `DirNode`。原 `RootDirectory` 使用了 `axfs_vfs::impl_vfs_dir_default!{}`，其默认 `rename` 返回 `Unsupported` —— 所以根本没走到 `DirNode::rename`。

**修复**：为 `exercise-ramfs-rename/axfs/src/root.rs` 的 `VfsNodeOps for RootDirectory` 显式实现 `rename`，把调用转发到实际挂载的文件系统（仿照 `create`/`remove`/`lookup` 的写法）：

```rust
fn rename(&self, src_path: &str, dst_path: &str) -> VfsResult {
    let src_norm = self.normalize_path(src_path);
    let dst_norm = self.normalize_path(dst_path);
    if let Some((mount_fs, src_rest)) = self.find_best_mount(src_norm) {
        let dst_rest = self
            .find_best_mount(dst_norm)
            .map(|(_, r)| r)
            .unwrap_or(dst_norm);
        mount_fs.root_dir().rename(src_rest, dst_rest)
    } else {
        self.main_fs.root_dir().rename(src_norm, dst_norm)
    }
}
```

**运行结果**：

```
Create directory '/tmp' ...
Create '/tmp/f1' and write [hello] ...
Read '/tmp/f1' content: [hello] ok!
Rename '/tmp/f1' to '/tmp/f2' ...
Read '/tmp/f2' content: [hello] ok!

[Ramfs-Rename]: ok!
```

---

### 练习 5：exercise-sysmap —— 无代码 bug，仅需工具链

`sys_mmap` 的 Rust 代码正确，**代码层面无 bug**。

**构建时的环境问题**：`xtask` 会调用 `{arch}-linux-musl-gcc` 编译 `payload/mapfile_c/mapfile.c`。macOS 下 Homebrew 无 `riscv64-linux-musl-gcc`（`messense/macos-cross-toolchains` 只提供 `aarch64/x86_64/i686` 三种）。

**解决**：使用 aarch64 架构测试，通过 brew 安装 `aarch64-unknown-linux-musl`（它同时提供 `aarch64-linux-musl-gcc` 软链接）：

```bash
brew tap messense/macos-cross-toolchains
brew install aarch64-unknown-linux-musl
cargo xtask run --arch aarch64
```

**运行结果**（QEMU aarch64 输出片段）：

```
Enter user space: entry=0x4001ac, ustack=VA:0x3fffffffa0
MapFile ...
handle_syscall [56] ...    (openat)
handle_syscall [222] ...   (mmap)
Read back content: hello, arceos!
MapFile ok!
[SYS_EXIT_GROUP]: exiting ..
monolithic kernel exit [Some(0)] normally!
```

用户程序通过 mmap 从映射地址读回文件内容，sys_mmap 实现正确。

---

### 修复文件清单

| 文件 | 修改内容 |
|------|---------|
| `exercise-hashmap/axstd/Cargo.toml` | `hashbrown` 追加 `default-hasher` feature |
| `exercise-altalloc/modules/bump_allocator/src/lib.rs` | 导入 `AllocError`，替换 3 处 `LinuxError::ENOMEM` → `AllocError::NoMemory` |
| `exercise-ramfs-rename/axfs_ramfs/src/dir.rs` | 修复 `navigate_to` 中的 `Arc<Self>` 调用 |
| `exercise-ramfs-rename/src/main.rs` | 修复 `io::Error` 的比较方式 |
| `exercise-ramfs-rename/axfs/src/root.rs` | `RootDirectory` 新增 `rename` 方法，转发到挂载 fs |

### 五个练习最终状态

所有五个练习在 macOS (Apple Silicon) 的 QEMU 上编译、运行、测试全部通过。共修复 **5 处 bug**（2 个 feature/依赖类，1 个类型错误，1 个 trait 调用错误，1 个 VFS 转发缺失）。