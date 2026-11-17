{
  description = "Stockhouse NixOS module evaluation and VM qualification";

  inputs = {
    stockhouse.url = "path:..";
    nixpkgs.follows = "stockhouse/nixpkgs";
  };

  outputs = {
    stockhouse,
    nixpkgs,
    ...
  }: let
    system = "x86_64-linux";
    pkgs = nixpkgs.legacyPackages.${system};
    modules = stockhouse.nixosModules;
    moduleArgs = {inherit pkgs nixpkgs modules;};
    vmArgs = {
      inherit pkgs;
      module = modules.default;
    };
  in {
    checks.${system} =
      import ./module-proxy.test.nix moduleArgs
      // import ./module-ingest.test.nix moduleArgs
      // import ./vm-proxy.test.nix vmArgs
      // import ./vm-substitution.test.nix vmArgs
      // import ./vm-durability.test.nix vmArgs
      // import ./vm-lifecycle.test.nix vmArgs;
  };
}
