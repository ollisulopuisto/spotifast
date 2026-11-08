"""Avoid artifact packaging for credential-transport-only PRs."""
import os
import subprocess

SAFE = {
    ".github/workflows/release.yml", ".github/workflows/credential-transport.yml",
    ".github/workflows/macos-build.yml", ".github/workflows/packaging.yml",
    "docs/release-credential-transport.md", "tests/manual/test_keychain_transport.py",
    "packaging/macos/credential_transport.rb", "packaging/macos/secure_native_packages.rb",
    "packaging/macos/macos_ci_credentials.py", "packaging/macos/test_credential_transport.rb",
    "packaging/macos/select_release_checks.py",
}
if os.environ.get("GITHUB_EVENT_NAME") != "pull_request" or not os.environ.get("BASE"):
    print("package=true")
else:
    paths = subprocess.check_output(["git", "diff", "--name-only", os.environ["BASE"] + "...HEAD"], text=True).splitlines()
    print("package=" + str(not paths or any(p not in SAFE for p in paths)).lower())
