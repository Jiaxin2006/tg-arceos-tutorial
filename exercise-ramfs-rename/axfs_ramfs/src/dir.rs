use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::{string::String, vec::Vec};

use axfs_vfs::{VfsDirEntry, VfsNodeAttr, VfsNodeOps, VfsNodeRef, VfsNodeType};
use axfs_vfs::{VfsError, VfsResult};
use spin::RwLock;

use crate::file::FileNode;

/// The directory node in the RAM filesystem.
///
/// It implements [`axfs_vfs::VfsNodeOps`].
pub struct DirNode {
    this: Weak<DirNode>,
    parent: RwLock<Weak<dyn VfsNodeOps>>,
    children: RwLock<BTreeMap<String, VfsNodeRef>>,
}

impl DirNode {
    pub(super) fn new(parent: Option<Weak<dyn VfsNodeOps>>) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            this: this.clone(),
            parent: RwLock::new(parent.unwrap_or_else(|| Weak::<Self>::new())),
            children: RwLock::new(BTreeMap::new()),
        })
    }

    pub(super) fn set_parent(&self, parent: Option<&VfsNodeRef>) {
        *self.parent.write() = parent.map_or(Weak::<Self>::new() as _, Arc::downgrade);
    }

    /// Returns a string list of all entries in this directory.
    pub fn get_entries(&self) -> Vec<String> {
        self.children.read().keys().cloned().collect()
    }

    /// Checks whether a node with the given name exists in this directory.
    pub fn exist(&self, name: &str) -> bool {
        self.children.read().contains_key(name)
    }

    /// Creates a new node with the given name and type in this directory.
    pub fn create_node(&self, name: &str, ty: VfsNodeType) -> VfsResult {
        if self.exist(name) {
            log::error!("AlreadyExists {name}");
            return Err(VfsError::AlreadyExists);
        }
        let node: VfsNodeRef = match ty {
            VfsNodeType::File => Arc::new(FileNode::new()),
            VfsNodeType::Dir => Self::new(Some(self.this.clone())),
            _ => return Err(VfsError::Unsupported),
        };
        self.children.write().insert(name.into(), node);
        Ok(())
    }

    /// Navigate to the directory node at the given relative path.
    /// Returns the directory node at that path.
    pub fn navigate_to(&self, path: &str) -> VfsResult<VfsNodeRef> {
        let this = self.this.upgrade().ok_or(VfsError::NotFound)?;
        if path.is_empty() || path == "." {
            return Ok(this as VfsNodeRef);
        }
        (this as VfsNodeRef).lookup(path)
    }

    /// Removes a node by the given name in this directory.
    pub fn remove_node(&self, name: &str) -> VfsResult {
        let mut children = self.children.write();
        let node = children.get(name).ok_or(VfsError::NotFound)?;
        if let Some(dir) = node.as_any().downcast_ref::<DirNode>() {
            if !dir.children.read().is_empty() {
                return Err(VfsError::DirectoryNotEmpty);
            }
        }
        children.remove(name);
        Ok(())
    }
}

impl VfsNodeOps for DirNode {
    fn get_attr(&self) -> VfsResult<VfsNodeAttr> {
        Ok(VfsNodeAttr::new_dir(4096, 0))
    }

    fn parent(&self) -> Option<VfsNodeRef> {
        self.parent.read().upgrade()
    }

    fn lookup(self: Arc<Self>, path: &str) -> VfsResult<VfsNodeRef> {
        let (name, rest) = split_path(path);
        let node = match name {
            "" | "." => Ok(self.clone() as VfsNodeRef),
            ".." => self.parent().ok_or(VfsError::NotFound),
            _ => self
                .children
                .read()
                .get(name)
                .cloned()
                .ok_or(VfsError::NotFound),
        }?;

        if let Some(rest) = rest {
            node.lookup(rest)
        } else {
            Ok(node)
        }
    }

    fn read_dir(&self, start_idx: usize, dirents: &mut [VfsDirEntry]) -> VfsResult<usize> {
        let children = self.children.read();
        let mut children = children.iter().skip(start_idx.max(2) - 2);
        for (i, ent) in dirents.iter_mut().enumerate() {
            match i + start_idx {
                0 => *ent = VfsDirEntry::new(".", VfsNodeType::Dir),
                1 => *ent = VfsDirEntry::new("..", VfsNodeType::Dir),
                _ => {
                    if let Some((name, node)) = children.next() {
                        *ent = VfsDirEntry::new(name, node.get_attr().unwrap().file_type());
                    } else {
                        return Ok(i);
                    }
                }
            }
        }
        Ok(dirents.len())
    }

    fn create(&self, path: &str, ty: VfsNodeType) -> VfsResult {
        log::debug!("create {ty:?} at ramfs: {path}");
        let (name, rest) = split_path(path);
        if let Some(rest) = rest {
            match name {
                "" | "." => self.create(rest, ty),
                ".." => self.parent().ok_or(VfsError::NotFound)?.create(rest, ty),
                _ => {
                    let subdir = self
                        .children
                        .read()
                        .get(name)
                        .ok_or(VfsError::NotFound)?
                        .clone();
                    subdir.create(rest, ty)
                }
            }
        } else if name.is_empty() || name == "." || name == ".." {
            Ok(()) // already exists
        } else {
            self.create_node(name, ty)
        }
    }

    fn remove(&self, path: &str) -> VfsResult {
        log::debug!("remove at ramfs: {path}");
        let (name, rest) = split_path(path);
        if let Some(rest) = rest {
            match name {
                "" | "." => self.remove(rest),
                ".." => self.parent().ok_or(VfsError::NotFound)?.remove(rest),
                _ => {
                    let subdir = self
                        .children
                        .read()
                        .get(name)
                        .ok_or(VfsError::NotFound)?
                        .clone();
                    subdir.remove(rest)
                }
            }
        } else if name.is_empty() || name == "." || name == ".." {
            Err(VfsError::InvalidInput) // remove '.' or '..
        } else {
            self.remove_node(name)
        }
    }

    fn rename(&self, src_path: &str, dst_path: &str) -> VfsResult {
        log::debug!("rename at ramfs: {} -> {}", src_path, dst_path);
        // Parse src path to get (parent_dir_path, src_name)
        let src_path = src_path.trim_start_matches('/');
        let dst_path = dst_path.trim_start_matches('/');

        // Find the source: navigate to the parent of src, get the node
        let (src_parent, src_name) = split_last_component(src_path);
        let (dst_parent, dst_name) = split_last_component(dst_path);

        // Navigate to src parent directory
        let src_dir = self.navigate_to(src_parent)?;
        // Navigate to dst parent directory
        let dst_dir = self.navigate_to(dst_parent)?;

        // Check if source and destination are the same directory (same-directory rename)
        let is_same_dir = core::ptr::eq(
            src_dir.as_ref() as *const dyn VfsNodeOps as *const (),
            dst_dir.as_ref() as *const dyn VfsNodeOps as *const (),
        );

        if let Some(src_dir_concrete) = src_dir.as_any().downcast_ref::<DirNode>() {
            if is_same_dir {
                // Same directory: use single write lock
                let node = src_dir_concrete
                    .children
                    .write()
                    .remove(src_name)
                    .ok_or(VfsError::NotFound)?;
                src_dir_concrete
                    .children
                    .write()
                    .insert(dst_name.into(), node);
            } else {
                // Different directories (shouldn't happen in this exercise but handle it)
                let node = src_dir_concrete
                    .children
                    .write()
                    .remove(src_name)
                    .ok_or(VfsError::NotFound)?;
                if let Some(dst_dir_concrete) = dst_dir.as_any().downcast_ref::<DirNode>() {
                    dst_dir_concrete
                        .children
                        .write()
                        .insert(dst_name.into(), node);
                } else {
                    // Put it back
                    src_dir_concrete
                        .children
                        .write()
                        .insert(src_name.into(), node);
                    return Err(VfsError::Unsupported);
                }
            }
            Ok(())
        } else {
            Err(VfsError::Unsupported)
        }
    }

    axfs_vfs::impl_vfs_dir_default! {}
}

fn split_path(path: &str) -> (&str, Option<&str>) {
    let trimmed_path = path.trim_start_matches('/');
    trimmed_path.find('/').map_or((trimmed_path, None), |n| {
        (&trimmed_path[..n], Some(&trimmed_path[n + 1..]))
    })
}

/// Split a path into (parent_dir_path, filename).
/// e.g., "tmp/f1" -> ("tmp", "f1"), "f1" -> ("", "f1")
fn split_last_component(path: &str) -> (&str, &str) {
    let path = path.trim_end_matches('/');
    if let Some(pos) = path.rfind('/') {
        let parent = &path[..pos];
        let name = &path[pos + 1..];
        (parent, name)
    } else {
        ("", path)
    }
}
