{ pkgs, config, lib, ... }:

let
  cfg = config.languages.haskell;

  # Wrapper for stack that configures it to use devenv's GHC
  stackWrapper = pkgs.runCommand "stack-wrapper"
    {
      buildInputs = [ pkgs.makeWrapper ];
    } ''
    mkdir -p $out/bin
    makeWrapper ${cfg.stack.package}/bin/stack $out/bin/stack \
      ${lib.concatMapStringsSep " \\\n      " (arg: "--add-flags \"${arg}\"") cfg.stack.args}
  '';
  # ghc.version with removed dots
  ghcVersion = lib.replaceStrings [ "." ] [ "" ] cfg.package.version;
in
{
  options.languages.haskell = {
    enable = lib.mkEnableOption "tools for Haskell development";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.ghc;
      defaultText = lib.literalExpression "pkgs.ghc";
      description = ''
        Haskell compiler to use.
      '';
    };

    languageServer = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = pkgs.haskell-language-server.override
        {
          supportedGhcVersions = [ ghcVersion ];
        };
      defaultText = lib.literalExpression "pkgs.haskell-language-server";
      description = ''
        Haskell language server to use.
      '';
    };

    stack = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = '' Whether to enable the Haskell Stack      '';
      };

      package = lib.mkOption {
        type = lib.types.package;
        default = pkgs.stack;
        defaultText = lib.literalExpression "pkgs.stack";
        description = ''
          Haskell stack package to use.
        '';
      };

      args = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ "--no-nix" "--system-ghc" "--no-install-ghc" ];
        defaultText = lib.literalExpression ''[ "--no-nix" "--system-ghc" "--no-install-ghc" ]'';
        description = ''
          Additional arguments to pass to stack.
          By default, stack is configured to use devenv's GHC installation.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    packages = with pkgs; [
      cfg.package
      cabal-install
      zlib
      hpack
    ]
    ++ (lib.optional (cfg.languageServer != null) cfg.languageServer)
    ++ (lib.optional cfg.stack.enable stackWrapper);
  };
}
