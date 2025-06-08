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

use std::collections::VecDeque;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use futures::FutureExt as _;
use gix::config::tree::Core;
use gix::filter::plumbing::eol::AutoCrlf;
use gix::filter::plumbing::eol::Stats;
use tokio::io::AsyncReadExt as _;

use super::TargetEol;
use super::TargetEolStrategy;
use super::PROBE_SIZE;
use crate::backend::FileId;
use crate::git_backend::GitBackend;
use crate::repo_path::RepoPath;
use crate::store::Store;

pub(super) struct GitTargetEolStrategy {
    store: Arc<Store>,
    auto_crlf: Option<AutoCrlf>,
}

impl GitTargetEolStrategy {
    pub fn new(store: Arc<Store>, git_backend: &GitBackend) -> Self {
        let config = git_backend.git_config();
        let auto_crlf = config
            .raw_value(Core::AUTO_CRLF)
            .ok()
            .and_then(|auto_crlf| Core::AUTO_CRLF.try_into_autocrlf(auto_crlf).ok());
        Self {
            store: Arc::clone(&store),
            auto_crlf,
        }
    }
}

impl TargetEolStrategy for GitTargetEolStrategy {
    fn get_snapshot_reader_target_eol(
        &self,
        file_path: &Path,
        content: &mut Box<dyn std::io::Read + Send>,
    ) -> TargetEol {
        fn is_file_binary(file: &mut Box<dyn std::io::Read + Send>) -> Option<bool> {
            let mut first = file.take(PROBE_SIZE as u64);
            let mut content = Vec::with_capacity(PROBE_SIZE);
            let result = first.read_to_end(&mut content).ok().map(|_| {
                let stats = Stats::from_bytes(&content);
                stats.is_binary()
            });
            replace_with::replace_with(
                file,
                || Box::new([].as_slice()),
                |file| {
                    let cached_file = Read::chain(VecDeque::from(content), file);
                    Box::new(cached_file)
                },
            );
            result
        }

        if let Some(auto_crlf) = self.auto_crlf {
            match auto_crlf {
                AutoCrlf::Disabled => return TargetEol::PassThrough,
                AutoCrlf::Enabled | AutoCrlf::Input => {
                    match is_file_binary(content) {
                        Some(true) => return TargetEol::PassThrough,
                        Some(false) => return TargetEol::Lf,
                        None => {
                            // Fall through to the default.
                        }
                    }
                }
            }
        }

        super::DefaultTargetEolStrategy.get_snapshot_reader_target_eol(file_path, content)
    }

    fn get_update_writer_target_eol<'a>(
        &'a self,
        repo_path: &'a RepoPath,
        file_id: &'a FileId,
    ) -> futures::future::BoxFuture<'a, TargetEol> {
        async fn is_file_binary(
            repo_path: &RepoPath,
            file_id: &FileId,
            store: &Store,
        ) -> Option<bool> {
            let reader = store.read_file(repo_path, file_id).await.ok()?;
            let mut content = Vec::with_capacity(PROBE_SIZE);
            reader
                .take(PROBE_SIZE as u64)
                .read_to_end(&mut content)
                .await
                .ok()?;
            let stats = Stats::from_bytes(&content);
            Some(stats.is_binary())
        }

        async {
            if let Some(auto_crlf) = self.auto_crlf {
                match auto_crlf {
                    AutoCrlf::Disabled | AutoCrlf::Input => return TargetEol::PassThrough,
                    AutoCrlf::Enabled => {
                        match is_file_binary(repo_path, file_id, &self.store).await {
                            Some(true) => return TargetEol::PassThrough,
                            Some(false) => return TargetEol::Crlf,
                            None => {
                                // Fall through to the default.
                            }
                        }
                    }
                }
            }
            super::DefaultTargetEolStrategy
                .get_update_writer_target_eol(repo_path, file_id)
                .await
        }
        .boxed()
    }
}
