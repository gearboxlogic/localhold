use std::path::Path;

use super::{filesystem, normalize_continuations};

pub(in super::super) fn has_opaque_filesystem_write(path: &str, source: &str) -> bool {
    filesystem::has_opaque_write(&normalize_continuations(source)) && !filesystem::is_reviewed_dynamic_write_surface(path, source)
}

pub(in super::super) fn has_opaque_filesystem_write_in_workspace(workspace: &Path, execution_surfaces: &[String], path: &str, source: &str) -> bool {
    filesystem::has_opaque_write_in_workspace(workspace, execution_surfaces, &normalize_continuations(source)) && !filesystem::is_reviewed_dynamic_write_surface(path, source)
}
