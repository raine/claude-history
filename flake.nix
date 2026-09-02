{
  description = "Fuzzy-search Claude Code conversation history from the terminal.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = cargoToml.package.name;
            version = cargoToml.package.version;

            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            buildInputs = [ pkgs.onnxruntime ];

            ORT_LIB_PATH = "${pkgs.lib.getLib pkgs.onnxruntime}/lib";
            ORT_PREFER_DYNAMIC_LINK = "1";

            # Some tests require filesystem access not available in Nix sandbox
            doCheck = false;

            doInstallCheck = true;
            nativeInstallCheckInputs = [ pkgs.versionCheckHook ];
            versionCheckProgramArg = "--version";

            meta = with pkgs.lib; {
              description = "Fuzzy-search Claude Code conversation history from the terminal.";
              homepage = "https://github.com/raine/claude-history";
              license = licenses.mit;
              mainProgram = "claude-history";
            };
          };
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/claude-history";
        };
      });

      devShells = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            buildInputs = with pkgs; [
              cargo
              rustc
              rust-analyzer
              rustfmt
              clippy
            ];

            RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";
          };
        }
      );
    };
}
