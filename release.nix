{
  stdenv,
  fetchurl,
  autoPatchelfHook,
  system,
  pkgs,
  rustPlatform ? pkgs.rustPlatform,
}:
let
  version = (fromTOML (builtins.readFile ./Cargo.toml)).package.version;
  maybeRemote = builtins.tryEval (fetchurl {
    url = "https://github.com/hexrustox/nix-capsule/releases/download/${version}/${system}.tar.gz";
    hash = "sha256-ftsOswc9wKPmeSvMF3qOVD8dpX2HqFf011nh1B8k4/M=";
  });
in
if maybeRemote.success then
  stdenv.mkDerivation {
    pname = "ncap";
    inherit version;
    src = maybeRemote.value;
    sourceRoot = ".";
    nativeBuildInputs = [ autoPatchelfHook ];
    buildInputs = [ stdenv.cc.cc.lib ];
    installPhase = ''
      mkdir $out
      cp -r bin share $out/
    '';
  }
else
  import ./package.nix { inherit pkgs rustPlatform; }
