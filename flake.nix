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
              cargo-watch
              rustc
              rustfmt
              clippy
              git
              pkg-config
              fontconfig
              nerd-fonts.jetbrains-mono
              noto-fonts-cjk-sans
              freetype
              libxkbcommon
              wayland
              vulkan-loader
              libglvnd
              wireplumber
              ffmpeg
            ];

            # GPUI's Wayland renderer loads Vulkan at runtime.  Nix store paths
            # are not in the dynamic linker's default lookup path.
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibraries;

          };
        });
    };
}
