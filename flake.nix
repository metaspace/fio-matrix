{
  description = "Run fio workloads";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/release-24.05";
    flake-utils.url = "github:numtide/flake-utils";
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
        flake-utils.follows = "flake-utils";
      };
    };
  };

  outputs = inputs:
    with inputs;
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        fio-matrix =
          pkgs.callPackage ./. { inherit nixpkgs system crane rust-overlay; };
      in {
        packages.default = fio-matrix;
        devShells.default = let
          overlays = [ (import inputs.rust-overlay) ];
          pkgs = import inputs.nixpkgs { inherit overlays system; };
        in pkgs.mkShell {
          packages = [ pkgs.rust-bin.stable.latest.complete ];
        };
      });
}
