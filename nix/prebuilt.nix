{
  system,
  stdenv,
  fetchurl,
  autoPatchelfHook,
  gcc,
}:

let
  cargoToml = fromTOML (builtins.readFile ../Cargo.toml);
  version = cargoToml.package.version;

  hashes = import ./prebuilt-hashes.nix;
  hash = hashes.${system} or (throw "ncap-prebuilt: no prebuilt artifact for ${system}; use packages.${system}.default or the from-source overlay");
in
stdenv.mkDerivation {
  inherit version;
  pname = "ncap";

  src = fetchurl {
    url = "https://github.com/hexrustox/nix-capsule/releases/download/v${version}/${system}.tar.gz";
    inherit hash;
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
