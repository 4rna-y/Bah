#!/usr/bin/env bash
# Build Bah and install the executable used by the Hyprland key bindings.
set -euo pipefail

readonly repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly install_dir="${BAH_INSTALL_DIR:-${XDG_BIN_HOME:-$HOME/.local/bin}}"
readonly runtime_dir="${BAH_RUNTIME_DIR:-$HOME/.local/lib/bah}"
readonly source_binary="$repo_dir/target/release/bah"
readonly runtime_binary="$runtime_dir/bah"
readonly installed_binary="$install_dir/bah"
readonly compatibility_binary="$install_dir/Bah"

build() {
    if [[ -n "${IN_NIX_SHELL:-}" ]]; then
        cargo build --release
    elif command -v nix >/dev/null 2>&1; then
        nix develop "$repo_dir" --command cargo build --release
    else
        cargo build --release
    fi
}

runtime_library_path() {
    if [[ -n "${IN_NIX_SHELL:-}" && -n "${LD_LIBRARY_PATH:-}" ]]; then
        printf '%s' "$LD_LIBRARY_PATH"
    elif command -v nix >/dev/null 2>&1; then
        nix develop "$repo_dir" --command bash -c 'printf %s "$LD_LIBRARY_PATH"'
    else
        printf '%s' "${LD_LIBRARY_PATH:-}"
    fi
}

cd "$repo_dir"
build

runtime_libraries="$(runtime_library_path)"
if [[ -z "$runtime_libraries" ]]; then
    printf '%s\n' 'No runtime library path was available; run this script through nix develop.' >&2
    exit 1
fi

mkdir -p "$install_dir" "$runtime_dir"
install -m 0755 "$source_binary" "$runtime_binary"
# GPUI loads Wayland and Vulkan dynamically. The small launcher preserves the
# Nix development shell's runtime library lookup when Hyprland starts Bah.
printf '#!/usr/bin/env bash\nset -euo pipefail\nreadonly runtime_libraries=%q\nexport LD_LIBRARY_PATH="${runtime_libraries}${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"\nexec %q "$@"\n' \
    "$runtime_libraries" "$runtime_binary" >"$installed_binary"
chmod 0755 "$installed_binary"
ln -sfn "bah" "$compatibility_binary"

printf 'Installed Bah: %s\n' "$installed_binary"
printf 'Runtime executable: %s\n' "$runtime_binary"
printf 'Compatibility command: %s\n' "$compatibility_binary"
printf 'Hyprland bindings use this absolute path; run hyprctl reload after changing them.\n'
