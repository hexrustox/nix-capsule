{
  stdenv,
  fetchurl,
  autoPatchelfHook,
  system,
  pkgs,
  rustPlatform ? pkgs.rustPlatform,
  useCache ? true,
}:
let
  version = (fromTOML (builtins.readFile ./Cargo.toml)).package.version;
  cachedSystems = [
    "x86_64-linux"
  ];
in
if useCache && (builtins.elem system cachedSystems) then
  stdenv.mkDerivation {
    pname = "ncap";
    inherit version;
    src = fetchurl {
      url = "https://github.com/hexrustox/nix-capsule/releases/download/v${version}/${system}.tar.gz";
      hash = "sha256-ftsOswc9wKPmeSvMF3qOVD8dpX2HqFf011nh1B8k4/M=";
    };
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
