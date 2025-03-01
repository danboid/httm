//       ___           ___           ___           ___
//      /\__\         /\  \         /\  \         /\__\
//     /:/  /         \:\  \        \:\  \       /::|  |
//    /:/__/           \:\  \        \:\  \     /:|:|  |
//   /::\  \ ___       /::\  \       /::\  \   /:/|:|__|__
//  /:/\:\  /\__\     /:/\:\__\     /:/\:\__\ /:/ |::::\__\
//  \/__\:\/:/  /    /:/  \/__/    /:/  \/__/ \/__/~~/:/  /
//       \::/  /    /:/  /        /:/  /            /:/  /
//       /:/  /     \/__/         \/__/            /:/  /
//      /:/  /                                    /:/  /
//      \/__/                                     \/__/
//
// Copyright (c) 2023, Robert Swinford <robert.swinford<...at...>gmail.com>
//
// For the full copyright and license information, please view the LICENSE file
// that was distributed with this source code.

use crate::data::paths::PathData;
use crate::data::paths::PathDeconstruction;
use crate::library::diff_copy::HttmCopy;
use crate::library::results::{HttmError, HttmResult};
use nix::sys::stat::SFlag;
use nu_ansi_term::Color::{Blue, Red};
use std::fs::{self, File, FileTimes};
use std::os::unix::fs::chown;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;

use std::fs::{create_dir_all, read_dir, set_permissions};
use std::iter::Iterator;
use std::path::Path;

pub struct Copy;

impl Copy {
    pub fn generate_dst_parent(dst: &Path) -> HttmResult<()> {
        if let Some(dst_parent) = dst.parent() {
            create_dir_all(dst_parent)?;
            Ok(())
        } else {
            let msg = format!("Could not detect a parent for destination file: {:?}", dst);
            Err(HttmError::new(&msg).into())
        }
    }

    pub fn direct(src: &Path, dst: &Path, should_preserve: bool) -> HttmResult<()> {
        Self::direct_quiet(src, dst, should_preserve)?;
        eprintln!("{}: {:?} -> {:?}", Blue.paint("Restored "), src, dst);

        Ok(())
    }

    pub fn direct_quiet(src: &Path, dst: &Path, should_preserve: bool) -> HttmResult<()> {
        if src.is_dir() {
            create_dir_all(&dst)?;
        } else {
            Self::generate_dst_parent(&dst)?;

            if src.is_file() {
                HttmCopy::new(&src, &dst)?;
            } else {
                if dst.exists() {
                    Remove::recursive_quiet(dst)?;
                }
                if src.is_symlink() {
                    let link_target = std::fs::read_link(&src)?;
                    std::os::unix::fs::symlink(&link_target, &dst)?;
                } else {
                    Self::special_file(src, dst)?;
                }
            }
        }

        if should_preserve {
            Preserve::direct(src, dst)?
        }

        Ok(())
    }

    fn special_file(src: &Path, dst: &Path) -> HttmResult<()> {
        const CHAR_KIND: SFlag = SFlag::from_bits_truncate(libc::S_IFCHR);
        const BLK_KIND: SFlag = SFlag::from_bits_truncate(libc::S_IFBLK);

        let src_metadata = src.metadata()?;
        let src_file_type = src_metadata.file_type();
        let src_mode_bits = src_metadata.mode();
        #[cfg(target_os = "linux")]
        let dst_mode = nix::sys::stat::Mode::from_bits_truncate(src_mode_bits);
        #[cfg(any(target_os = "macos", target_os = "freebsd"))]
        let dst_mode = nix::sys::stat::Mode::from_bits_truncate(src_mode_bits as u16);

        let is_blk = src_file_type.is_block_device();
        let is_char = src_file_type.is_char_device();
        let is_fifo = src_file_type.is_fifo();
        let is_socket = src_file_type.is_socket();

        if is_blk || is_char {
            let dev = src_metadata.dev();
            let kind = if is_blk { BLK_KIND } else { CHAR_KIND };
            #[cfg(target_os = "linux")]
            nix::sys::stat::mknod(dst, kind, dst_mode, dev)?;
            #[cfg(target_os = "macos")]
            nix::sys::stat::mknod(dst, kind, dst_mode, dev as i32)?;
            #[cfg(target_os = "freebsd")]
            nix::sys::stat::mknod(dst, kind, dst_mode, dev as u32)?;
        } else if is_fifo {
            // create new fifo
            nix::unistd::mkfifo(dst, dst_mode)?;
        } else if is_socket {
            let msg = format!(
            "WARN: Source path could not be copied.  Source path is a socket, and sockets are not considered within the scope of httm.  \
            Traditionally, sockets could not be copied, and they should always be recreated by the generating daemon, when deleted: \"{}\"",
            src.display()
        );
            eprintln!("{}", msg)
        } else {
            let msg = format!(
            "httm could not determine the source path's file type, and therefore it could not be copied.  \
            The source path was not recognized as a directory, regular file, device, fifo, socket, or symlink.  \
            Other special file types (like doors and event ports) are unsupported: \"{}\"",
            src.display()
        );
            return Err(HttmError::new(&msg).into());
        }

        Ok(())
    }

    pub fn recursive(src: &Path, dst: &Path, should_preserve: bool) -> HttmResult<()> {
        if src.is_dir() {
            Self::direct(src, dst, should_preserve)?;

            for entry in read_dir(&src)?.flatten() {
                let file_type = entry.file_type()?;
                let entry_src = entry.path();
                let entry_dst = dst.join(entry.file_name());

                if entry_src.exists() {
                    if file_type.is_dir() {
                        Self::recursive(&entry_src, &entry_dst, should_preserve)?;
                    } else {
                        Self::direct(&entry_src, &entry_dst, should_preserve)?;
                    }
                }
            }
        } else {
            Self::direct(&src, dst, should_preserve)?;
        }

        if should_preserve {
            Preserve::recursive(src, dst)?
        }

        Ok(())
    }
}

pub struct Preserve;

impl Preserve {
    pub fn direct(src: &Path, dst: &Path) -> HttmResult<()> {
        let src_metadata = src.symlink_metadata()?;

        // Mode
        {
            set_permissions(dst, src_metadata.permissions())?
        }

        // ACLs - requires libacl1-dev to build
        #[cfg(feature = "acls")]
        {
            if let Ok(acls) = exacl::getfacl(src, None) {
                exacl::setfacl(&[dst], &acls, None)?;
            }
        }

        // Ownership
        {
            let dst_uid = src_metadata.uid();
            let dst_gid = src_metadata.gid();

            chown(dst, Some(dst_uid), Some(dst_gid))?
        }

        // XAttrs
        {
            #[cfg(feature = "xattrs")]
            if let Ok(xattrs) = xattr::list(src) {
                xattrs
                    .flat_map(|attr| {
                        xattr::get(src, attr.clone()).map(|opt_value| (attr, opt_value))
                    })
                    .filter_map(|(attr, opt_value)| opt_value.map(|value| (attr, value)))
                    .try_for_each(|(attr, value)| xattr::set(dst, attr, value.as_slice()))?
            }
        }

        // Timestamps
        {
            let src = fs::metadata(src)?;
            let dst_file = File::options().write(true).open(dst)?;
            let times = FileTimes::new()
                .set_accessed(src.accessed()?)
                .set_modified(src.modified()?);
            dst_file.set_times(times)?;
        }

        Ok(())
    }

    pub fn recursive(src: &Path, dst: &Path) -> HttmResult<()> {
        let dst_pathdata: PathData = dst.into();

        let proximate_dataset_mount = dst_pathdata.proximate_dataset()?;

        let Ok(relative_path) = dst_pathdata.relative_path(proximate_dataset_mount) else {
            let msg = format!(
                "Could not determine relative path for destination: {:?}",
                dst
            );
            return Err(HttmError::new(&msg).into());
        };

        let relative_path_components_len = relative_path.components().count();

        src.ancestors()
            .zip(dst.ancestors())
            .take(relative_path_components_len)
            .try_for_each(|(src_ancestor, dst_ancestor)| {
                Preserve::direct(src_ancestor, dst_ancestor)
            })
    }
}

pub struct Remove;

impl Remove {
    pub fn recursive(src: &Path) -> HttmResult<()> {
        Self::recursive_quiet(src)?;

        eprintln!("{}: {:?} -> 🗑️", Red.paint("Removed  "), src);

        Ok(())
    }

    pub fn recursive_quiet(src: &Path) -> HttmResult<()> {
        if src.is_dir() {
            for entry in read_dir(&src)?.flatten() {
                let file_type = entry.file_type()?;
                let path = entry.path();

                if path.exists() {
                    if file_type.is_dir() {
                        Self::recursive(&path)?;
                    } else {
                        std::fs::remove_file(path)?
                    }
                }
            }

            if src.exists() {
                std::fs::remove_dir_all(&src)?
            }
        } else if src.exists() {
            std::fs::remove_file(&src)?
        }

        Ok(())
    }
}
