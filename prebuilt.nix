{
  stdenv,
  fetchurl,
  autoPatchelfHook,
  gcc,
}:

let
  system = "x86_64-linux";
  cargoToml = fromTOML (builtins.readFile ./Cargo.toml);
  version = cargoToml.package.version;
  hash = "";
in
stdenv.mkDerivation {
  inherit version;
  pname = "ncap";

  src = fetchurl {
    inherit hash;
    url = "https://github.com/hexrustox/nix-capsule/releases/download/v${version}/${system}.tar.gz";
  };

  nativeBuildInputs = [ autoPatchelfHook ];
  buildInputs = [ gcc.cc.lib ];

  phases = [ "installPhase" ];

  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    tar -xzf $src
    cp -r bin share "$out"
    runHook postInstall
  '';
}
