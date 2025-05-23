set -xe

rm devenv.yaml || true
devenv shell -- env|grep "DEVENV_CMDLINE=shell"
devenv build languages.python.package
devenv shell ls -- -la | grep ".test.sh"
devenv shell ls ../ | grep "cli"
devenv info | grep "python3-"
devenv show | grep "python3-"
devenv search ncdu |& grep -E "Found [0-9]+ packages and [0-9]+ options for 'ncdu'"

# there should be no processes
devenv up && exit 1

# Skip container tests on macOS
if [[ "$(uname)" == "Darwin" ]]; then
  echo "Skipping container tests on macOS"
  exit 0
fi

# containers
devenv container build shell && exit 1
devenv inputs add mk-shell-bin github:rrbutani/nix-mk-shell-bin --follows nixpkgs
devenv inputs add nix2container github:nlewo/nix2container --follows nixpkgs
devenv container build shell | grep image-shell.json
# bw compat
devenv container shell | grep "image-shell.json"
devenv gc
