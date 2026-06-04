{
  stdenv,
  autoPatchelfHook,
  system,
}:
stdenv.mkDerivation {
  pname = "ncap";
  version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
  src = ./. + "/releases/${system}.tar.gz";
  sourceRoot = ".";
  preferLocalBuild = true;
  nativeBuildInputs = [ autoPatchelfHook ];
  buildInputs = [ stdenv.cc.cc.lib ];
  installPhase = ''
    mkdir $out
    cp -r bin share $out/
  '';
}
