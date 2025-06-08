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

use std::fs::File;
use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt as _;
use gix::attrs::State;
use gix::config::tree::Core;
use gix::filter::plumbing::eol::AutoCrlf;
use gix::filter::plumbing::eol::Stats;
use gix::glob::wildmatch::Mode;
use tokio::io::AsyncReadExt as _;

use super::TargetEol;
use super::TargetEolStrategy;
use crate::backend::FileId;
use crate::backend::TreeValue;
use crate::git_backend::GitBackend;
use crate::merged_tree::MergedTree;
use crate::repo_path::RepoPath;
use crate::repo_path::RepoPathBuf;
use crate::repo_path::RepoPathComponent;
use crate::store::Store;

// Read at most 8KB to decide whether this file is binary.
const PROBE_SIZE: usize = 8 << 10;

#[derive(Clone)]
pub(super) struct GitTargetEolStrategy {
    store: Arc<Store>,
    auto_crlf: Option<AutoCrlf>,
    git_attributes: Arc<GitAttributesFile>,
}

#[derive(Clone)]
struct GitAttributeLine {
    pattern: gix::glob::Pattern,
    eol: Option<State>,
    text: Option<State>,
}

#[derive(Clone)]
/// EOL related git attributes.
struct GitAttributesFile {
    parent: Option<Arc<GitAttributesFile>>,
    prefix: RepoPathBuf,
    lines: Vec<GitAttributeLine>,
}

impl Default for GitAttributesFile {
    fn default() -> Self {
        Self {
            parent: None,
            prefix: RepoPathBuf::root(),
            lines: vec![],
        }
    }
}

impl GitAttributesFile {
    pub fn chain(self: &Arc<GitAttributesFile>, prefix: &RepoPath, content: &[u8]) -> Arc<GitAttributesFile> {
        let mut lines = vec![];
        for line in gix::attrs::parse(&content) {
            let Ok((gix::attrs::parse::Kind::Pattern(pattern), attrs, _)) = line else {
                // TODO: handle macro definition.
                continue;
            };
            let mut text = None;
            let mut eol = None;
            for attr in attrs {
                // TODO: also handle the crlf attribute and binary macro here.
                let Ok(attr) = attr else {
                    continue;
                };
                if attr.name.as_str() == "text" {
                    // TODO: implement: specifying eol automatically sets text if text was left unspecified
                    text = Some(attr.state.to_owned());
                }
                if attr.name.as_str() == "eol" {
                    eol = Some(attr.state.to_owned());
                }
            }
            if text.is_none() && eol.is_none() {
                continue;
            }
            lines.push(GitAttributeLine { pattern, eol, text });
        }
        if lines.is_empty() {
            Arc::clone(self)
        } else {
            Arc::new(Self {
                parent: Some(Arc::clone(self)),
                prefix: prefix.to_owned(),
                lines,
            })
        }
    }

    pub fn visit_matches<'a, T>(&'a self, path: &RepoPath, mut visitor: impl FnMut(&'a GitAttributeLine) -> Option<T>) -> Option<T> {
        for file in std::iter::successors(Some(self), |file| file.parent.as_deref()) {
            let Some(rest_path) = path.strip_prefix(&file.prefix) else {
                continue;
            };
            for line in file.lines.iter().rev() {
                if !line.pattern.matches(rest_path.as_internal_file_string().as_bytes().into(), Mode::empty()) {
                    continue;
                }
                let Some(ret) = visitor(line) else {
                    continue;
                };
                return Some(ret);
            }
        }
        None
    }
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
            // TODO: handle the global and repo level gitattributes.
            git_attributes: Default::default(),
        }
    }
}

impl TargetEolStrategy for GitTargetEolStrategy {
    fn get_snapshot_reader_target_eol(&self, file_path: &Path) -> TargetEol {
        fn is_file_binary(file_path: &Path) -> Option<bool> {
            let file = File::options().read(true).open(file_path).unwrap();

            let mut first = file.take(PROBE_SIZE as u64);
            let mut content = Vec::with_capacity(PROBE_SIZE);
            first.read_to_end(&mut content).ok()?;
            let stats = Stats::from_bytes(&content);
            Some(stats.is_binary())
        }

        if let Some(auto_crlf) = self.auto_crlf {
            match auto_crlf {
                AutoCrlf::Disabled => return TargetEol::PassThrough,
                AutoCrlf::Enabled | AutoCrlf::Input => {
                    match is_file_binary(file_path) {
                        Some(true) => return TargetEol::PassThrough,
                        Some(false) => return TargetEol::Lf,
                        None => {
                            // Fall through to the default.
                        }
                    }
                }
            }
        }

        super::DefaultTargetEolStrategy.get_snapshot_reader_target_eol(file_path)
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
            // TODO: handle the case where eol is unset or has an invalid value.
            let git_attr_eol = self.git_attributes.visit_matches(repo_path, |line| line.eol.as_ref());
            // TODO: handle the case where text is set to a value, but is not "auto".
            let git_attr_text = self.git_attributes.visit_matches(repo_path, |line| line.text.as_ref());
            match git_attr_text {
                Some(State::Set) => {
                    match git_attr_eol {
                        None => {
                            // TODO: handle the git core.eol config.
                            fn native_target_eol() -> TargetEol {
                                if cfg!(windows) {
                                    TargetEol::Crlf
                                } else {
                                    TargetEol::Lf
                                }
                            }
                            return native_target_eol();
                        }
                        Some(_) => todo!(),
                    }
                },
                Some(State::Unset) => todo!(),
                Some(State::Value(value)) => {
                    debug_assert_eq!(value.as_ref().as_bstr(), b"auto");
                    todo!()
                },
                Some(State::Unspecified) | None => {
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
                }
            }
            super::DefaultTargetEolStrategy
                .get_update_writer_target_eol(repo_path, file_id)
                .await
        }
        .boxed()
    }

    fn add_dir_layer_from_store<'a>(&'a self, repo_path: &'a RepoPath, tree: &'a MergedTree) -> BoxFuture<'a, Arc<dyn TargetEolStrategy>> {
        let default_ret = || -> Arc<dyn TargetEolStrategy> { Arc::new(self.clone()) };
        let gitattributes_path = RepoPathBuf::from_internal_string(".gitattributes").expect("this is a valid path");
        async move {
            let Ok(git_attribute_file) = tree.path_value_async(&gitattributes_path).await else {
                return default_ret();
            };
            let Some(Some(TreeValue::File { id: git_attribute_file_id, .. })) = git_attribute_file.as_resolved() else {
                return default_ret();
            };
            println!("!!!! {}", repo_path.to_internal_dir_string());
            let git_attribute_repo_path = repo_path.to_owned().join(&RepoPathComponent::new(".gitattributes").expect("this is a valid path component"));
            let Ok(mut file_reader) = self.store.read_file(&git_attribute_repo_path, git_attribute_file_id).await else {
                return default_ret();
            };
            let mut git_attribute_content = vec![];
            if let Err(_) = file_reader.read_to_end(&mut git_attribute_content).await {
                return default_ret();
            }
            Arc::new(Self {
                git_attributes: self.git_attributes.chain(repo_path, &git_attribute_content),
                ..self.clone()
            })
        }.boxed()
    }

    fn add_dir_layer_from_disk(&self, _disk_path: &Path) -> Arc<dyn TargetEolStrategy> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
}
