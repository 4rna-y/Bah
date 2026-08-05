{
  description = "A GPUI layer-shell status bar for Hyprland";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forEachSystem = nixpkgs.lib.genAttrs systems;
    in
    {
      devShells = forEachSystem (system:
        let
          pkgs = import nixpkgs { inherit system; };
          runtimeLibraries = with pkgs; [
            fontconfig
            freetype
            libxkbcommon
            wayland
            vulkan-loader
            libglvnd
          ];
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              git
              patch
              pkg-config
              fontconfig
              nerd-fonts.jetbrains-mono
              freetype
              libxkbcommon
              wayland
              vulkan-loader
              libglvnd
            ];

            # GPUI's Wayland renderer loads Vulkan at runtime.  Nix store paths
            # are not in the dynamic linker's default lookup path.
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibraries;

            # Keep the patched Git source isolated from the user's global
            # Cargo cache while retaining the pinned upstream revision.
            shellHook = ''
              export CARGO_HOME="$PWD/.cargo"
              cargo fetch --locked

              gpui_checkout=$(find "$CARGO_HOME/git/checkouts" -path '*/4aad57f' -type d -print -quit)
              if [ -z "$gpui_checkout" ]; then
                echo "hyprbar: pinned GPUI checkout was not fetched" >&2
                return 1
              fi

              gpui_marker="$gpui_checkout/.hyprbar-layer-shell-stretch-applied"
              if [ ! -e "$gpui_marker" ]; then
                patch -d "$gpui_checkout" -p1 < "$PWD/patches/gpui-layer-shell-stretch.patch"
                touch "$gpui_marker"
              fi
            '';
          };
        });
    };
}
