// Copyright 2025 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[cfg(feature = "git")]
mod git;
mod read;
mod write;

use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt as _;
pub(crate) use read::ReadExt;
pub(crate) use write::WriteExt;

use crate::backend::FileId;
use crate::merged_tree::MergedTree;
#[cfg(feature = "git")]
use crate::git_backend::GitBackend;
use crate::repo_path::RepoPath;
use crate::store::Store;

/// The target EOL to convert to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TargetEol {
    /// Do not convert EOL.
    PassThrough,
    /// Convert to CRLF (Carriage Return Line Feed, `0x0D 0x0A`, `\r\n`).
    #[allow(
        unused,
        reason = "if the git feature is not enabled, the CRLF target EOL won't be used"
    )]
    Crlf,
    /// Convert to LF (Line Feed, `0x0A`, `\n`).
    #[allow(
        unused,
        reason = "if the git feature is not enabled, the LF target EOL won't be used"
    )]
    Lf,
}

pub(crate) trait TargetEolStrategy: Sync + Send {
    fn get_snapshot_reader_target_eol(&self, file_path: &Path) -> TargetEol;
    fn get_update_writer_target_eol<'a>(
        &'a self,
        repo_path: &'a RepoPath,
        file_id: &'a FileId,
    ) -> BoxFuture<'a, TargetEol>;
    fn add_dir_layer_from_store<'a>(&'a self, repo_path: &'a RepoPath, tree: &'a MergedTree) -> BoxFuture<'a, Arc<dyn TargetEolStrategy>>;
    fn add_dir_layer_from_disk(&self, disk_path: &Path) -> Arc<dyn TargetEolStrategy>;
}

pub(crate) fn get_target_eol_strategy(
    #[allow(
        unused,
        reason = "if the git feature isn't enabled, the store won't be used"
    )]
    store: &Arc<Store>,
) -> Box<dyn TargetEolStrategy> {
    #[cfg(feature = "git")]
    if let Some(git_backend) = store.backend_impl().downcast_ref::<GitBackend>() {
        return Box::new(git::GitTargetEolStrategy::new(
            Arc::clone(store),
            git_backend,
        ));
    }
    Box::new(DefaultTargetEolStrategy)
}

struct DefaultTargetEolStrategy;

impl TargetEolStrategy for DefaultTargetEolStrategy {
    fn get_snapshot_reader_target_eol(&self, _file_path: &Path) -> TargetEol {
        TargetEol::PassThrough
    }

    fn get_update_writer_target_eol<'a>(
        &'a self,
        _repo_path: &'a RepoPath,
        _file_id: &'a FileId,
    ) -> BoxFuture<'a, TargetEol> {
        async { TargetEol::PassThrough }.boxed()
    }

    fn add_dir_layer_from_store<'a>(&'a self, _repo_path: &'a RepoPath, _tree: &'a MergedTree) -> BoxFuture<'a, Arc<dyn TargetEolStrategy>> {
        async { Arc::new(Self) as Arc<dyn TargetEolStrategy> }.boxed()
    }

    fn add_dir_layer_from_disk(&self, _disk_path: &Path) -> Arc<dyn TargetEolStrategy> {
        Arc::new(Self)
    }
}
