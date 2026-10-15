#!/usr/bin/env bats

setup() {
	bats_load_library xberg-bats
	xberg_setup
	REPO_ROOT="$(cd "$BATS_TEST_DIRNAME/../../.." && pwd -P)"
	SCRIPT="$REPO_ROOT/scripts/ci/rust/run-unit-tests.sh"
	xberg_stub_trace cargo
}

assert_unit_test_commands() {
	local core_command="$1"
	local workspace_command="cargo test --workspace"
	workspace_command+=" --exclude crawlberg --exclude crawlberg-py --exclude crawlberg-node"
	workspace_command+=" --exclude crawlberg-php --exclude crawlberg-wasm --exclude crawlberg-cli"
	workspace_command+=" --all-features --no-fail-fast --verbose"

	run env REPO_ROOT="$REPO_ROOT" "$SCRIPT"

	xberg_assert_status 0
	xberg_assert_trace \
		"$core_command" \
		"$workspace_command" \
		"cargo test -p crawlberg-cli --features all --no-fail-fast --verbose"
}

@test "run-unit-tests should_pass_no_fail_fast_to_all_three_test_runs_on_linux" {
	xberg_stub uname 'printf "%s\n" Linux'

	assert_unit_test_commands "cargo test -p crawlberg --all-features --no-fail-fast --verbose"
}

@test "run-unit-tests should_optimize_only_the_core_test_profile_on_darwin" {
	xberg_stub uname 'printf "%s\n" Darwin'
	local core_command="cargo test -p crawlberg --all-features --no-fail-fast --verbose"
	core_command+=" --config profile.test.package.crawlberg.opt-level=1"

	assert_unit_test_commands "$core_command"
}
