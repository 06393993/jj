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

use std::error::Error;
use std::fs::File;
use std::io::Cursor;
use std::io::Read;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use bstr::ByteSlice as _;
use futures::future::BoxFuture;
use futures::FutureExt as _;
use gix::filter::plumbing::eol::Stats;
use thiserror::Error;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt as _;

use crate::backend::FileId;
use crate::config::ConfigGetError;
use crate::config::ConfigValue;
use crate::repo_path::RepoPath;
use crate::settings::UserSettings;
use crate::store::Store;

#[derive(Error, Debug)]
#[error("{message}")]
struct EolError {
    message: String,
    #[source]
    source: Option<Box<dyn Error + Send + Sync>>,
}

pub(crate) trait TargetEolStrategy: Send + Sync {
    fn get_target_eol_for_snapshot(
        &self,
        file_path: &Path,
    ) -> Result<TargetEol, Box<dyn Error + Send + Sync>>;
    fn get_target_eol_for_update<'a>(
        &'a self,
        repo_path: &'a RepoPath,
        file_id: &'a FileId,
    ) -> BoxFuture<'a, Result<TargetEol, Box<dyn Error + Send + Sync>>>;
}

pub(crate) fn create_target_eol_strategy(
    store: Arc<Store>,
    user_settings: &UserSettings,
) -> Result<impl TargetEolStrategy + 'static, impl Error + Send + Sync> {
    let eol_conversion_settings = EolConversionSettings::try_get_from_settings(user_settings)?;
    Ok::<_, EolError>(TargetEolStrategyImpl {
        store,
        eol_conversion_settings,
    })
}

struct TargetEolStrategyImpl {
    store: Arc<Store>,
    eol_conversion_settings: EolConversionSettings,
}

impl TargetEolStrategyImpl {
    const PROBE_LIMIT: u64 = 8 << 10;
}

impl TargetEolStrategy for TargetEolStrategyImpl {
    fn get_target_eol_for_snapshot(
        &self,
        file_path: &Path,
    ) -> Result<TargetEol, Box<dyn Error + Send + Sync>> {
        match self.eol_conversion_settings {
            EolConversionSettings::None => Ok(TargetEol::PassThrough),
            EolConversionSettings::Input | EolConversionSettings::InputOutput => {
                let file = File::options()
                    .read(true)
                    .open(file_path)
                    .map_err(|source| {
                        Box::new(EolError {
                            message: format!("failed to open the file {}", file_path.display()),
                            source: Some(Box::new(source)),
                        })
                    })?;
                let mut content = vec![];
                file.take(Self::PROBE_LIMIT)
                    .read_to_end(&mut content)
                    .map_err(|source| {
                        Box::new(EolError {
                            message: format!(
                                "failed to read from the file {}",
                                file_path.display()
                            ),
                            source: Some(Box::new(source)),
                        })
                    })?;
                if Stats::from_bytes(&content).is_binary() {
                    return Ok(TargetEol::PassThrough);
                }
                Ok(TargetEol::Lf)
            }
        }
    }
    fn get_target_eol_for_update<'a>(
        &'a self,
        repo_path: &'a RepoPath,
        file_id: &'a FileId,
    ) -> BoxFuture<'a, Result<TargetEol, Box<dyn Error + Send + Sync>>> {
        async {
            match self.eol_conversion_settings {
                EolConversionSettings::None | EolConversionSettings::Input => {
                    Ok(TargetEol::PassThrough)
                }
                EolConversionSettings::InputOutput => {
                    let mut content = vec![];
                    self.store
                        .read_file(repo_path, file_id)
                        .await
                        .map_err(|source| {
                            Box::new(EolError {
                                message: format!(
                                    "failed to create reader to the {} file from the store",
                                    repo_path.as_internal_file_string()
                                ),
                                source: Some(Box::new(source)),
                            })
                        })?
                        .take(Self::PROBE_LIMIT)
                        .read_to_end(&mut content)
                        .await
                        .map_err(|source| {
                            Box::new(EolError {
                                message: format!(
                                    "failed to read the {} file from the store",
                                    repo_path.as_internal_file_string()
                                ),
                                source: Some(Box::new(source)),
                            })
                        })?;
                    if Stats::from_bytes(&content).is_binary() {
                        return Ok(TargetEol::PassThrough);
                    }
                    Ok(TargetEol::Crlf)
                }
            }
        }
        .boxed()
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
enum EolConversionSettings {
    None,
    Input,
    InputOutput,
}

impl EolConversionSettings {
    fn try_from_config_value(value: ConfigValue) -> Result<Self, impl Error + Send + Sync> {
        let value = value.as_str().ok_or_else(|| EolError {
            message: "the working-copy.eol-conversion setting can't be casted to a string"
                .to_string(),
            source: None,
        })?;
        match value {
            "none" => Ok(Self::None),
            "input" => Ok(Self::Input),
            "input-output" => Ok(Self::InputOutput),
            other => Err(EolError {
                message: format!("unrecognized working-copy.eol-conversion value: {other}"),
                source: None,
            }),
        }
    }
    fn try_get_from_settings(user_settings: &UserSettings) -> Result<Self, EolError> {
        match user_settings
            .get_value_with("working-copy.eol-conversion", Self::try_from_config_value)
        {
            Ok(value) => Ok(value),
            Err(ConfigGetError::NotFound { .. }) => Ok(Self::None),
            Err(source) => Err(EolError {
                message: "failed to retrieve the working-copy.eol-conversion setting".to_string(),
                source: Some(Box::new(source)),
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetEol {
    Lf,
    Crlf,
    PassThrough,
}

fn convert_eol_inner(input: &[u8], target_eol: TargetEol) -> Vec<u8> {
    let eol = match target_eol {
        TargetEol::Lf => b"\n".as_slice(),
        TargetEol::Crlf => b"\r\n".as_slice(),
        TargetEol::PassThrough => unimplemented!(
            "The caller should handle this case to avoid unnecessary copy and allocation"
        ),
    };
    let mut lines = input.lines().peekable();
    let mut res = Vec::<u8>::with_capacity(input.len());
    while let Some(line) = lines.next() {
        res.extend_from_slice(line);
        if lines.peek().is_some() || input.last() == Some(&b'\n') {
            // If we are not the last line, we should append the EOL, because this line must
            // ends with an EOL. If we are the last line, we only append the EOL when the
            // last line ends with EOL.
            res.extend_from_slice(eol);
        }
    }
    res
}

struct ErrorReader(Option<std::io::Error>);

impl ErrorReader {
    fn new(error: std::io::Error) -> Self {
        Self(Some(error))
    }
}

impl Read for ErrorReader {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(e) = self.0.take() {
            return Err(e);
        }
        Ok(0)
    }
}

impl AsyncRead for ErrorReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if let Some(e) = self.0.take() {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(()))
    }
}

pub(crate) fn convert_eol<'a>(
    mut input: impl Read + Send + 'a,
    target_eol: TargetEol,
) -> impl Read + Send + 'a {
    if target_eol == TargetEol::PassThrough {
        return Box::new(input) as Box<dyn Read + Send>;
    }
    let mut content = vec![];
    if let Err(e) = input.read_to_end(&mut content) {
        return Box::new(ErrorReader::new(e)) as Box<dyn Read + Send>;
    }
    let res = convert_eol_inner(&content, target_eol);
    Box::new(Cursor::new(res)) as Box<dyn Read + Send>
}

pub(crate) async fn convert_eol_async<'a>(
    mut input: impl AsyncRead + Send + Unpin + 'a,
    target_eol: TargetEol,
) -> impl AsyncRead + Send + Unpin + 'a {
    if target_eol == TargetEol::PassThrough {
        return Box::pin(input) as Pin<Box<dyn AsyncRead + Send + Unpin>>;
    }
    let mut content = vec![];
    if let Err(e) = input.read_to_end(&mut content).await {
        return Box::pin(ErrorReader::new(e)) as Pin<Box<dyn AsyncRead + Send + Unpin>>;
    }
    let res = convert_eol_inner(&content, target_eol);
    Box::pin(Cursor::new(res)) as Pin<Box<dyn AsyncRead + Send + Unpin>>
}

#[cfg(test)]
mod tests {
    use pollster::FutureExt as _;
    use test_case::test_case;
    use tokio::io::AsyncReadExt as _;

    use super::*;
    use crate::config::ConfigLayer;
    use crate::config::ConfigSource;
    use crate::config::StackedConfig;

    #[test_case(b"a\n", TargetEol::PassThrough, b"a\n"; "LF text with no EOL conversion")]
    #[test_case(b"a\r\n", TargetEol::PassThrough, b"a\r\n"; "CRLF text with no EOL conversion")]
    #[test_case(b"a", TargetEol::PassThrough, b"a"; "no EOL text with no EOL conversion")]
    #[test_case(b"a\n", TargetEol::Crlf, b"a\r\n"; "LF text with CRLF EOL conversion")]
    #[test_case(b"a\r\n", TargetEol::Crlf, b"a\r\n"; "CRLF text with CRLF EOL conversion")]
    #[test_case(b"a", TargetEol::Crlf, b"a"; "no EOL text with CRLF conversion")]
    #[test_case(b"", TargetEol::Crlf, b""; "empty text with CRLF EOL conversion")]
    #[test_case(b"a\nb", TargetEol::Crlf, b"a\r\nb"; "text ends without EOL with CRLF EOL conversion")]
    #[test_case(b"a\n", TargetEol::Lf, b"a\n"; "LF text with LF EOL conversion")]
    #[test_case(b"a\r\n", TargetEol::Lf, b"a\n"; "CRLF text with LF EOL conversion")]
    #[test_case(b"a", TargetEol::Lf, b"a"; "no EOL text with LF conversion")]
    #[test_case(b"", TargetEol::Lf, b""; "empty text with LF EOL conversion")]
    #[test_case(b"a\r\nb", TargetEol::Lf, b"a\nb"; "text ends without EOL with LF EOL conversion")]
    fn test_eol_conversion(input: &[u8], target_eol: TargetEol, expected_output: &[u8]) {
        {
            let mut input = input;
            let mut output = vec![];
            convert_eol(&mut input, target_eol)
                .read_to_end(&mut output)
                .expect("failed to read the output to end");
            assert_eq!(output, expected_output);
        }

        async {
            let mut input = input;
            let mut output = vec![];
            convert_eol_async(&mut input, target_eol)
                .await
                .read_to_end(&mut output)
                .await
                .expect("failed to read the output to end");
            assert_eq!(output, expected_output);
        }
        .block_on();
    }

    #[test_case(TargetEol::PassThrough; "no EOL conversion")]
    #[test_case(TargetEol::Lf; "LF EOL conversion")]
    #[test_case(TargetEol::Crlf; "CRLF EOL conversion")]
    fn test_eol_convert_eol_read_error(target_eol: TargetEol) {
        let message = "test error";
        let error_reader = ErrorReader::new(std::io::Error::other(message));
        let mut eol_converted = convert_eol(error_reader, target_eol);
        let mut buf = [0; 1];
        let e = eol_converted.read(&mut buf).expect_err("should fail");
        assert!(
            e.to_string().contains(message),
            "the error message must contain the original error message"
        );
    }

    #[test_case(TargetEol::PassThrough; "no EOL conversion")]
    #[test_case(TargetEol::Lf; "LF EOL conversion")]
    #[test_case(TargetEol::Crlf; "CRLF EOL conversion")]
    fn test_eol_convert_eol_async_read_error(target_eol: TargetEol) {
        async {
            let message = "test error";
            let error_reader = ErrorReader::new(std::io::Error::other(message));
            let mut eol_converted = convert_eol_async(error_reader, target_eol).await;
            let mut buf = [0; 1];
            let e = eol_converted.read(&mut buf).await.expect_err("should fail");
            assert!(
                e.to_string().contains(message),
                "the error message must contain the original error message"
            );
        }
        .block_on();
    }

    fn user_settings_from_toml_text(config_text: &str) -> UserSettings {
        let mut config = StackedConfig::with_defaults();
        let default_config_text = r#"
            user.name = "Test User"
            user.email = "test.user@example.com"
            operation.username = "test-username"
            operation.hostname = "host.example.com"
            debug.randomness-seed = 42
        "#;
        config.add_layer(ConfigLayer::parse(ConfigSource::User, default_config_text).unwrap());
        config.add_layer(
            ConfigLayer::parse(ConfigSource::User, config_text)
                .expect("failed to parse the config text"),
        );
        UserSettings::from_config(config).expect("failed to create user settings from the config")
    }

    #[test]
    fn test_eol_conversion_setting_parse_should_default_to_none() {
        let user_settings = user_settings_from_toml_text("");
        let setting = EolConversionSettings::try_get_from_settings(&user_settings)
            .expect("should parse successfully");
        assert_eq!(setting, EolConversionSettings::None);
    }

    #[test_case(r#"working-copy.eol-conversion = "none""# => EolConversionSettings::None)]
    #[test_case(r#"working-copy.eol-conversion = "input""# => EolConversionSettings::Input)]
    #[test_case(r#"working-copy.eol-conversion = "input-output""# => EolConversionSettings::InputOutput)]
    fn test_eol_conversion_setting_parse_should_parse_correct_values_successfully(
        config_text: &str,
    ) -> EolConversionSettings {
        let user_settings = user_settings_from_toml_text(config_text);
        EolConversionSettings::try_get_from_settings(&user_settings)
            .expect("should parse successfully")
    }

    #[test_case("working-copy.eol-conversion = true"; "not string")]
    #[test_case(r#"working-copy.eol-conversion = "invalid-value-42""#; "invalid string")]
    fn test_eol_conversion_setting_parse_should_fail_on_invalid_values(config_text: &str) {
        let user_settings = user_settings_from_toml_text(config_text);
        EolConversionSettings::try_get_from_settings(&user_settings)
            .expect_err("should fail the parsing");
    }
}
