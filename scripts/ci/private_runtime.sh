# Source in a GitHub Actions test step so umask, environment and trap apply to the tests.
if [[ "$RUNNER_OS" == macOS ]]; then
  # The runner's home may have an extended ACL; do not change it.
  umask 077
  XDG_RUNTIME_DIR=$(sudo -n mktemp -d /Users/.taunicorn-ci.XXXXXXXX)
  trap 'sudo -n rm -rf -- "$XDG_RUNTIME_DIR"' EXIT
  sudo -n chown "$(id -u):$(id -g)" "$XDG_RUNTIME_DIR"
  export XDG_RUNTIME_DIR
elif [[ "$RUNNER_OS" != Windows ]]; then
  umask 077
  XDG_RUNTIME_DIR=$(mktemp -d "$HOME/.taunicorn-ci.XXXXXXXX")
  trap 'rm -rf -- "$XDG_RUNTIME_DIR"' EXIT
  export XDG_RUNTIME_DIR
fi
