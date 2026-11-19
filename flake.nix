{
  description = "wavectl: priority-aware audio manager for PipeWire (duck, pause, route)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        wavectl = import ./default.nix { inherit pkgs; };
        default = wavectl;
      });

      overlays.default = final: _prev: { wavectl = import ./default.nix { pkgs = final; }; };

      devShells = forAll (pkgs: { default = import ./shell.nix { inherit pkgs; }; });
    };
}
