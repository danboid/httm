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
// (c) Robert Swinford <robert.swinford<...at...>gmail.com>
//
// For the full copyright and license information, please view the LICENSE file
// that was distributed with this source code.

use crate::{Config, HttmError, PathData, SnapPoint};
use fxhash::FxHashMap as HashMap;
use rayon::prelude::*;
use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

pub fn lookup_exec(
    config: &Config,
    path_data: &Vec<PathData>,
) -> Result<[Vec<PathData>; 2], Box<dyn std::error::Error + Send + Sync + 'static>> {   
    // create vec of backups
    // let snapshot_versions: Vec<PathData> = path_data
    //     .par_iter()
    //     .map(|pathdata| get_versions(config, pathdata, &switch_dataset(config, pathdata, false)?))
    //     .flatten_iter()
    //     .flatten_iter()
    //     .collect();

    // create vec of backups
    //
    let snapshot_versions: Vec<PathData> = 
    //if config.opt_alt_root {
        path_data
            .par_iter()
            .map(|pathdata| get_versions(config, pathdata, &switch_dataset(config, pathdata, true)?))
            .flatten_iter()
            .flatten_iter()
            .collect();
    // } else {
    //     Vec::new()
    // };

    //let all_snaps: Vec<PathData> = [snapshot_versions, alt_replicated].into_iter().flatten().collect();

    // create vec of live copies - unless user doesn't want it!
    let live_versions: Vec<PathData> = if !config.opt_no_live_vers {
        path_data.to_owned()
    } else {
        Vec::new()
    };

    // check if all files (snap and live) do not exist, if this is true, then user probably messed up
    // and entered a file that never existed (that is, perhaps a wrong file name)?
    if snapshot_versions.is_empty() && live_versions.iter().all(|i| i.is_phantom) {
        return Err(HttmError::new(
            "Neither a live copy, nor a snapshot copy of such a file appears to exist, so, umm, 🤷? Please try another file.",
        )
        .into());
    }

    Ok([snapshot_versions, live_versions])
}

pub fn get_snap_point_and_local_relative_path(
    config: &Config,
    path: &Path,
    og_dataset: &Path,
    dataset: &Path,
) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error + Send + Sync + 'static>> {
    // building the snapshot path from our dataset
    let snapshot_dir: PathBuf = [dataset, Path::new(".zfs/snapshot")].iter().collect();

    // building our local relative path by removing parent
    // directories below the remote/snap mount point
    let local_path = match &config.snap_point {
        SnapPoint::UserDefined(defined_dirs) => {
            path
            .strip_prefix(&defined_dirs.local_dir).map_err(|_| HttmError::new("Are you sure you're in the correct working directory?  Perhaps you need to set the LOCAL_DIR value."))
        }
        SnapPoint::Native(_) => {
            let parent = path.parent().unwrap();
            if path.ancestors().all(|dataset| dataset != parent) {
                let mut res = Ok(path);
                for parent in path.ancestors() {
                    let hidden_snap_dir: PathBuf = [parent, Path::new(".zfs")].iter().collect();
                    if hidden_snap_dir.exists() {
                        res = path.strip_prefix(&parent).map_err(|_| HttmError::new("Are you sure you're in the correct working directory?  Perhaps you need to set the SNAP_DIR and LOCAL_DIR values."));
                    } else {
                        continue
                    }
                }
                println!("{:?}", res);
                res
            } else {
                path
                .strip_prefix(&dataset).map_err(|_| HttmError::new("Are you sure you're in the correct working directory?  Perhaps you need to set the SNAP_DIR and LOCAL_DIR values."))   
            }

        }
    }?;

    Ok((snapshot_dir, local_path.to_path_buf()))
}

fn switch_dataset(
    config: &Config,
    pathdata: &PathData,
    for_alt_root: bool,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync + 'static>> {
    // which ZFS dataset do we want to use
    let dataset = match &config.snap_point {
        SnapPoint::UserDefined(defined_dirs) => defined_dirs.snap_dir.to_owned(),
        SnapPoint::Native(mount_collection) => {
            let standard_mount = get_snapshot_dataset(pathdata, mount_collection)?;
            if for_alt_root {
                
                let mut unique_mounts: HashMap<&Path, &String> = HashMap::default();
                
                // reverse the order - mount as key, fs as value
                mount_collection.into_iter().for_each(|(fs, mount)| {
                    let _ = unique_mounts.insert(Path::new(mount), fs);
                });

                // so we can search for the mount as key
                let standard_fs_name = if let Some(name) = unique_mounts.get(standard_mount.as_path()) {
                    name.to_owned().to_owned()
                } else {
                    return Ok(standard_mount)
                };

                if let Some((alt_mount, _)) = unique_mounts.clone()
                    .into_par_iter()
                    .filter(|(_, fs)| fs.ends_with(standard_fs_name.as_str()))
                    .max_by_key(|(_,fs)| fs.len()) {
                        alt_mount.to_path_buf()
                    } else {
                        standard_mount
                    }
            } else {
                standard_mount
            }
        }
    };
    Ok(dataset)
}

fn get_versions(
    config: &Config,
    pathdata: &PathData,
    dataset: &Path,
) -> Result<Vec<PathData>, Box<dyn std::error::Error + Send + Sync + 'static>> {
    // generates path for hidden .zfs snap dir, and the corresponding local path
    let (hidden_snapshot_dir, local_path) =
        get_snap_point_and_local_relative_path(config, &pathdata.path_buf, &dataset)?;

    // get the DirEntry for our snapshot path which will have all our possible
    // needed snapshots
    let versions = std::fs::read_dir(hidden_snapshot_dir)?
        .flatten()
        .par_bridge()
        .map(|entry| entry.path())
        .map(|path| path.join(&local_path))
        .map(|path| PathData::from(path.as_path()))
        .filter(|pathdata| !pathdata.is_phantom)
        .collect::<Vec<PathData>>();

    // filter above will remove all the phantom values silently as we build a list of unique versions
    // and our hashmap will then remove duplicates with the same system modify time and size/file len
    let mut unique_versions: HashMap<(SystemTime, u64), PathData> = HashMap::default();
    versions.into_iter().for_each(|pathdata| {
        let _ = unique_versions.insert((pathdata.system_time, pathdata.size), pathdata);
    });

    let mut sorted: Vec<_> = unique_versions.into_iter().collect();

    sorted.par_sort_unstable_by_key(|&(k, _)| k);

    Ok(sorted.into_iter().map(|(_, v)| v).collect())
}

pub fn get_snapshot_dataset(
    pathdata: &PathData,
    mount_collection: &Vec<(String, String)>,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync + 'static>> {
    let file_path = &pathdata.path_buf;

    // only possible None is if root dir because
    // of previous work in the Pathdata new method
    let parent_folder = file_path.parent().unwrap_or_else(|| Path::new("/"));

    // prune away most datasets by filtering - parent folder of file must contain relevant dataset
    let potential_mountpoints: Vec<&String> = mount_collection
        .into_par_iter()
        .map(|(_, mount)| mount)
        .filter(|line| parent_folder.starts_with(line))
        .collect();

    // do we have any left? if not print error
    if potential_mountpoints.is_empty() {
        let msg = "Could not identify any qualifying dataset.  Maybe consider specifying manually at SNAP_POINT?";
        return Err(HttmError::new(msg).into());
    };

    // select the best match for us: the longest, as we've already matched on the parent folder
    // so for /usr/bin, we would then prefer /usr/bin to /usr and /
    let best_potential_mountpoint =
        if let Some(some_bpmp) = potential_mountpoints.par_iter().max_by_key(|x| x.len()) {
            some_bpmp
        } else {
            let msg = format!(
                "There is no best match for a ZFS dataset to use for path {:?}. Sorry!/Not sorry?)",
                file_path
            );
            return Err(HttmError::new(&msg).into());
        };

    Ok(PathBuf::from(best_potential_mountpoint))
}
