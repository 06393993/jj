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
use std::io::Write as _;

use bstr::ByteSlice as _;
use indoc::indoc;
use jj_lib::config::ConfigLayer;
use jj_lib::config::ConfigSource;
use jj_lib::file_util::check_symlink_support;
use jj_lib::file_util::try_symlink;
use jj_lib::repo::Repo as _;
use jj_lib::repo::StoreFactories;
use jj_lib::rewrite::merge_commit_trees;
use jj_lib::settings::UserSettings;
use jj_lib::working_copy::CheckoutOptions;
use jj_lib::workspace::default_working_copy_factories;
use jj_lib::workspace::Workspace;
use test_case::test_case;
use testutils::base_user_config;
use testutils::commit_with_tree;
use testutils::repo_path;
use testutils::TestRepoBackend;
use testutils::TestWorkspace;

static LF_FILE_CONTENT: &[u8] = b"aaa\nbbbb\nccccc\n";
static CRLF_FILE_CONTENT: &[u8] = b"aaa\r\nbbbb\r\nccccc\r\n";
static MIXED_EOL_FILE_CONTENT: &[u8] = b"aaa\nbbbb\r\nccccc\n";
static BINARY_FILE_CONTENT: &[u8] = include_bytes!("data/binary_file.png");

struct Config {
    extra_setting: &'static str,
    file_content: &'static [u8],
}

fn base_user_settings_with_extra_configs(extra_settings: &str) -> UserSettings {
    let mut config = base_user_config();
    config.add_layer(
        ConfigLayer::parse(ConfigSource::User, extra_settings)
            .expect("failed to parse the settings"),
    );
    UserSettings::from_config(config).expect("failed to create the UserSettings from the config")
}

#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input-output""#,
    file_content: LF_FILE_CONTENT,
} => LF_FILE_CONTENT; "eol-conversion input-output LF only file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input-output""#,
    file_content: CRLF_FILE_CONTENT,
} => LF_FILE_CONTENT; "eol-conversion input-output CRLF only file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input-output""#,
    file_content: MIXED_EOL_FILE_CONTENT,
} => LF_FILE_CONTENT; "eol-conversion input-output mixed EOL file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input-output""#,
    file_content: BINARY_FILE_CONTENT,
} => BINARY_FILE_CONTENT; "eol-conversion input-output binary file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input""#,
    file_content: LF_FILE_CONTENT,
} => LF_FILE_CONTENT; "eol-conversion input LF only file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input""#,
    file_content: CRLF_FILE_CONTENT,
} => LF_FILE_CONTENT; "eol-conversion input CRLF only file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input""#,
    file_content: MIXED_EOL_FILE_CONTENT,
} => LF_FILE_CONTENT; "eol-conversion input mixed EOL file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input""#,
    file_content: BINARY_FILE_CONTENT,
} => BINARY_FILE_CONTENT; "eol-conversion input binary file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "none""#,
    file_content: LF_FILE_CONTENT,
} => LF_FILE_CONTENT; "eol-conversion none LF only file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "none""#,
    file_content: CRLF_FILE_CONTENT,
} => CRLF_FILE_CONTENT; "eol-conversion none CRLF only file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "none""#,
    file_content: MIXED_EOL_FILE_CONTENT,
} => MIXED_EOL_FILE_CONTENT; "eol-conversion none mixed EOL file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "none""#,
    file_content: BINARY_FILE_CONTENT,
} => BINARY_FILE_CONTENT; "eol-conversion none binary file")]
fn test_eol_conversion_snapshot(
    Config {
        extra_setting,
        file_content,
    }: Config,
) -> Vec<u8> {
    let extra_setting = format!("{extra_setting}\n");
    let user_settings = base_user_settings_with_extra_configs(&extra_setting);
    let mut test_workspace =
        TestWorkspace::init_with_backend_and_settings(TestRepoBackend::Git, &user_settings);
    let file_repo_path = repo_path("test-eol-file");
    let file_disk_path = file_repo_path
        .to_fs_path(test_workspace.workspace.workspace_root())
        .unwrap();

    testutils::write_working_copy_file(
        test_workspace.workspace.workspace_root(),
        file_repo_path,
        file_content,
    );
    let tree = test_workspace.snapshot().unwrap();
    let new_tree = test_workspace.snapshot().unwrap();
    assert_eq!(
        new_tree.id(),
        tree.id(),
        "The working copy should be clean."
    );
    let file_added_commit = commit_with_tree(test_workspace.repo.store(), tree.id());

    std::fs::remove_file(&file_disk_path).unwrap();
    let tree = test_workspace.snapshot().unwrap();
    let file_removed_commit = commit_with_tree(test_workspace.repo.store(), tree.id());

    let workspace = &mut test_workspace.workspace;
    workspace
        .check_out(
            test_workspace.repo.op_id().clone(),
            None,
            &file_removed_commit,
            &CheckoutOptions::empty_for_test(),
        )
        .unwrap();
    assert!(!file_disk_path.exists());

    let user_settings =
        base_user_settings_with_extra_configs("working-copy.eol-conversion = \"none\"\n");

    let mut workspace = Workspace::load(
        &user_settings,
        test_workspace.workspace.workspace_root(),
        &StoreFactories::default(),
        &default_working_copy_factories(),
    )
    .expect("failed to reload the workspace");
    let file_added_commit = workspace
        .repo_loader()
        .store()
        .get_commit(file_added_commit.id())
        .expect("failed to find the commit with the test file");
    workspace
        .check_out(
            test_workspace.repo.op_id().clone(),
            None,
            &file_added_commit,
            &CheckoutOptions::empty_for_test(),
        )
        .unwrap();
    assert!(file_disk_path.exists());
    let new_tree = test_workspace.snapshot().unwrap();
    assert_eq!(
        new_tree.id(),
        *file_added_commit.tree_id(),
        "The working copy should be clean."
    );

    std::fs::read(&file_disk_path).expect("failed to read the checked out test file")
}

// Create a conflict commit in a CRLF EOL file, and append another line with the
// CRLF EOL to the file, create a snapshot on the modified merge conflict,
// checkout the snapshot with the given setting, and return the content of the
// file.
fn create_conflict_snapshot_and_read(extra_setting: &str) -> Vec<u8> {
    let no_eol_conversion_settings =
        base_user_settings_with_extra_configs("working-copy.eol-conversion = \"none\"\n");
    let mut test_workspace = TestWorkspace::init_with_backend_and_settings(
        TestRepoBackend::Git,
        &no_eol_conversion_settings,
    );
    let file_repo_path = repo_path("test-eol-file");
    let file_disk_path = file_repo_path
        .to_fs_path(test_workspace.workspace.workspace_root())
        .unwrap();

    // The commit graph:
    // C (conflict)
    // |\
    // A B
    // |/
    // (empty)
    let root_commit = test_workspace.repo.store().root_commit();
    testutils::write_working_copy_file(
        test_workspace.workspace.workspace_root(),
        file_repo_path,
        "a\r\n",
    );
    let tree = test_workspace.snapshot().unwrap();
    let mut tx = test_workspace.repo.start_transaction();
    let parent1_commit = tx
        .repo_mut()
        .new_commit(vec![root_commit.id().clone()], tree.id())
        .write()
        .unwrap();
    tx.commit("commit parent1").unwrap();

    test_workspace
        .workspace
        .check_out(
            test_workspace.repo.op_id().clone(),
            None,
            &root_commit,
            &CheckoutOptions::empty_for_test(),
        )
        .unwrap();
    testutils::write_working_copy_file(
        test_workspace.workspace.workspace_root(),
        file_repo_path,
        "b\r\n",
    );
    let tree = test_workspace.snapshot().unwrap();
    let mut tx = test_workspace.repo.start_transaction();
    let parent2_commit = tx
        .repo_mut()
        .new_commit(vec![root_commit.id().clone()], tree.id())
        .write()
        .unwrap();
    tx.commit("commit parent2").unwrap();

    test_workspace.repo = test_workspace.repo.reload_at_head().unwrap();
    assert!(test_workspace.repo.index().has_id(parent1_commit.id()));
    assert!(test_workspace.repo.index().has_id(parent2_commit.id()));
    let tree =
        merge_commit_trees(&*test_workspace.repo, &[parent1_commit, parent2_commit]).unwrap();
    let merge_commit = commit_with_tree(test_workspace.repo.store(), tree.id());

    let merge_commit = test_workspace
        .repo
        .store()
        .get_commit(merge_commit.id())
        .expect("failed to find the commit with the test file");
    test_workspace
        .workspace
        .check_out(
            test_workspace.repo.op_id().clone(),
            None,
            &merge_commit,
            &CheckoutOptions::empty_for_test(),
        )
        .unwrap();

    let mut file = File::options().append(true).open(&file_disk_path).unwrap();
    file.write_all(b"c\r\n").unwrap();
    drop(file);

    let extra_setting = format!("{extra_setting}\n");
    let user_settings = base_user_settings_with_extra_configs(&extra_setting);
    test_workspace.workspace = Workspace::load(
        &user_settings,
        test_workspace.workspace.workspace_root(),
        &StoreFactories::default(),
        &default_working_copy_factories(),
    )
    .expect("failed to reload the workspace");
    let tree = test_workspace.snapshot().unwrap();
    let new_tree = test_workspace.snapshot().unwrap();
    assert_eq!(
        new_tree.id(),
        tree.id(),
        "The working copy should be clean."
    );
    let merge_commit = commit_with_tree(test_workspace.repo.store(), tree.id());

    test_workspace.workspace = Workspace::load(
        &no_eol_conversion_settings,
        test_workspace.workspace.workspace_root(),
        &StoreFactories::default(),
        &default_working_copy_factories(),
    )
    .expect("failed to reload the workspace");

    test_workspace
        .workspace
        .check_out(
            test_workspace.repo.op_id().clone(),
            None,
            &test_workspace.workspace.repo_loader().store().root_commit(),
            &CheckoutOptions::empty_for_test(),
        )
        .unwrap();
    let merge_commit = test_workspace
        .workspace
        .repo_loader()
        .store()
        .get_commit(merge_commit.id())
        .expect("failed to find the commit with the test file");
    test_workspace
        .workspace
        .check_out(
            test_workspace.repo.op_id().clone(),
            None,
            &merge_commit,
            &CheckoutOptions::empty_for_test(),
        )
        .unwrap();

    assert!(std::fs::exists(&file_disk_path).unwrap());
    std::fs::read(&file_disk_path).unwrap()
}

#[test]
fn test_eol_conversion_input_output_snapshot_conflicts() {
    let contents =
        create_conflict_snapshot_and_read(r#"working-copy.eol-conversion = "input-output""#);
    for line in contents.lines_with_terminator() {
        assert!(
            !line.ends_with(b"\r\n"),
            "{:?} should not end with CRLF",
            line.to_str_lossy().as_ref()
        );
    }
}

#[test]
fn test_eol_conversion_input_snapshot_conflicts() {
    let contents = create_conflict_snapshot_and_read(r#"working-copy.eol-conversion = "input""#);
    for line in contents.lines_with_terminator() {
        assert!(
            !line.ends_with(b"\r\n"),
            "{:?} should not end with CRLF",
            line.to_str_lossy().as_ref()
        );
    }
}

#[test]
fn test_eol_conversion_none_snapshot_conflicts() {
    let contents = create_conflict_snapshot_and_read(r#"working-copy.eol-conversion = "none""#);
    // We only check the last line, because it is only guaranteed that the last line
    // is not the conflict markers.
    let line = contents.lines_with_terminator().next_back().unwrap();
    assert!(
        line.ends_with(b"\r\n"),
        "{:?} should end with CRLF",
        line.to_str_lossy().as_ref()
    );
}

#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input-output""#,
    file_content: LF_FILE_CONTENT,
} => CRLF_FILE_CONTENT; "eol-conversion input-output LF only file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input-output""#,
    file_content: CRLF_FILE_CONTENT,
} => CRLF_FILE_CONTENT; "eol-conversion input-output CRLF only file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input-output""#,
    file_content: MIXED_EOL_FILE_CONTENT,
} => CRLF_FILE_CONTENT; "eol-conversion input-output mixed EOL file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input-output""#,
    file_content: BINARY_FILE_CONTENT,
} => BINARY_FILE_CONTENT; "eol-conversion input-output binary file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input""#,
    file_content: LF_FILE_CONTENT,
} => LF_FILE_CONTENT; "eol-conversion input LF only file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input""#,
    file_content: CRLF_FILE_CONTENT,
} => CRLF_FILE_CONTENT; "eol-conversion input CRLF only file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input""#,
    file_content: MIXED_EOL_FILE_CONTENT,
} => MIXED_EOL_FILE_CONTENT; "eol-conversion input mixed EOL file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "input""#,
    file_content: BINARY_FILE_CONTENT,
} => BINARY_FILE_CONTENT; "eol-conversion input binary file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "none""#,
    file_content: LF_FILE_CONTENT,
} => LF_FILE_CONTENT; "eol-conversion none LF only file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "none""#,
    file_content: CRLF_FILE_CONTENT,
} => CRLF_FILE_CONTENT; "eol-conversion none CRLF only file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "none""#,
    file_content: MIXED_EOL_FILE_CONTENT,
} => MIXED_EOL_FILE_CONTENT; "eol-conversion none mixed EOL file")]
#[test_case(Config {
    extra_setting: r#"working-copy.eol-conversion = "none""#,
    file_content: BINARY_FILE_CONTENT,
} => BINARY_FILE_CONTENT; "eol-conversion none binary file")]
fn test_eol_conversion_checkout(
    Config {
        extra_setting,
        file_content,
    }: Config,
) -> Vec<u8> {
    let no_eol_conversion_settings =
        base_user_settings_with_extra_configs("working-copy.eol-conversion = \"none\"\n");
    let mut test_workspace = TestWorkspace::init_with_backend_and_settings(
        TestRepoBackend::Git,
        &no_eol_conversion_settings,
    );
    let file_repo_path = repo_path("test-eol-file");
    let file_disk_path = file_repo_path
        .to_fs_path(test_workspace.workspace.workspace_root())
        .unwrap();

    testutils::write_working_copy_file(
        test_workspace.workspace.workspace_root(),
        file_repo_path,
        file_content,
    );
    let tree = test_workspace.snapshot().unwrap();
    let commit = commit_with_tree(test_workspace.repo.store(), tree.id());

    test_workspace
        .workspace
        .check_out(
            test_workspace.repo.op_id().clone(),
            None,
            &test_workspace.workspace.repo_loader().store().root_commit(),
            &CheckoutOptions::empty_for_test(),
        )
        .unwrap();
    assert!(!std::fs::exists(&file_disk_path).unwrap());

    let extra_setting = format!("{extra_setting}\n");
    let user_settings = base_user_settings_with_extra_configs(&extra_setting);
    test_workspace.workspace = Workspace::load(
        &user_settings,
        test_workspace.workspace.workspace_root(),
        &StoreFactories::default(),
        &default_working_copy_factories(),
    )
    .expect("failed to reload the workspace");
    let commit = test_workspace
        .workspace
        .repo_loader()
        .store()
        .get_commit(commit.id())
        .expect("failed to find the commit with the test file");
    test_workspace
        .workspace
        .check_out(
            test_workspace.repo.op_id().clone(),
            None,
            &commit,
            &CheckoutOptions::empty_for_test(),
        )
        .unwrap();

    assert!(std::fs::exists(&file_disk_path).unwrap());
    std::fs::read(&file_disk_path).unwrap()
}

#[test]
fn test_eol_conversion_checkout_symlink_should_not_be_converted() {
    if !check_symlink_support().unwrap() {
        eprintln!("Skipping test because symlink isn't supported");
        return;
    }
    let no_eol_conversion_settings =
        base_user_settings_with_extra_configs("working-copy.eol-conversion = \"none\"\n");
    let mut test_workspace = TestWorkspace::init_with_backend_and_settings(
        TestRepoBackend::Git,
        &no_eol_conversion_settings,
    );
    let file_repo_path = repo_path("test-symlink");
    let file_disk_path = file_repo_path
        .to_fs_path(test_workspace.workspace.workspace_root())
        .unwrap();
    let target_file = "test\n\r\nfile";
    testutils::write_working_copy_file(
        test_workspace.workspace.workspace_root(),
        repo_path(target_file),
        "test content\n",
    );
    try_symlink(target_file, &file_disk_path).unwrap();
    let tree = test_workspace.snapshot().unwrap();
    let commit = commit_with_tree(test_workspace.repo.store(), tree.id());

    let user_settings = base_user_settings_with_extra_configs(indoc! {r#"
        debug.working-copy.symlink-support-override = false
        working-copy.eol-conversion = "input-output"
    "#});
    test_workspace.workspace = Workspace::load(
        &user_settings,
        test_workspace.workspace.workspace_root(),
        &StoreFactories::default(),
        &default_working_copy_factories(),
    )
    .expect("failed to reload the workspace");
    test_workspace
        .workspace
        .check_out(
            test_workspace.repo.op_id().clone(),
            None,
            &test_workspace.workspace.repo_loader().store().root_commit(),
            &CheckoutOptions::empty_for_test(),
        )
        .unwrap();
    assert!(!std::fs::exists(&file_disk_path).unwrap());
    let commit = test_workspace
        .workspace
        .repo_loader()
        .store()
        .get_commit(commit.id())
        .expect("failed to find the commit with the test file");
    test_workspace
        .workspace
        .check_out(
            test_workspace.repo.op_id().clone(),
            None,
            &commit,
            &CheckoutOptions::empty_for_test(),
        )
        .unwrap();
    assert!(std::fs::exists(&file_disk_path).unwrap());
    assert!(std::fs::symlink_metadata(&file_disk_path)
        .unwrap()
        .is_file());
    assert!(!std::fs::symlink_metadata(&file_disk_path)
        .unwrap()
        .is_symlink());
    let content = std::fs::read_to_string(file_disk_path).unwrap();
    assert!(
        content.ends_with(target_file),
        "{content:?} should end with {target_file:?}"
    );
}
