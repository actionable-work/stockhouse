{
  description = "Stockhouse: the open-source Linux cache client and NixOS module";

  inputs = {
    bloomery.url = "github:actionable-work/bloomery";
    nixpkgs.follows = "bloomery/nixpkgs";
    # Preserve the exact Nix versions required by VM-010.
    nixpkgs-vm-nix.url = "github:NixOS/nixpkgs/4975466d324710c576dc11ad614684e6bd8cad8e";
  };

  outputs = inputs @ {
    self,
    nixpkgs,
    bloomery,
    ...
  }: let
    # Only x86_64-linux NixOS is supported and VM-qualified.
    systems = ["x86_64-linux"];
    module = import ./nix/module.nix {
      defaultPackage = pkgs: self.packages.${pkgs.stdenv.hostPlatform.system}.stockhouse;
    };
  in
    bloomery.mkFlake {
      inherit nixpkgs systems;
      root = ./.;
      # Configured subflakes receive the live parent without another input pin.
      self.inputs = inputs // {stockhouse = self;};
      extraOutputs = _: {
        nixosModules = {
          default = module;
          client = module;
        };
      };
    };
}
