{
  description = "Kanbei control plane: the Deno kernel, its channels, and its evidence harness";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/241313f4e8e508cb9b13278c2b0fa25b9ca27163";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
      denoLockHash = "sha256-XRPAnW7troq8ld9rrLyR/XhcU/V6BMMsFnq7dr0Qqpo=";
    in
    {
      packages = forAllSystems (
        pkgs:
        let
          lib = pkgs.lib;

          # One fixed-output derivation vendors every remote module so the
          # runtime and every check run with no network. Recompute this hash
          # only by changing deno.lock and re-vendoring.
          denoDeps = pkgs.stdenvNoCC.mkDerivation {
            pname = "kanbei-deno-deps";
            version = "1";
            src = self;
            nativeBuildInputs = [
              pkgs.cacert
              pkgs.deno
            ];
            impureEnvVars = [
              "http_proxy"
              "https_proxy"
              "all_proxy"
              "no_proxy"
              "NIX_SSL_CERT_FILE"
            ];
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
            outputHashMode = "recursive";
            outputHashAlgo = "sha256";
            outputHash = denoLockHash;
            buildPhase = ''
              runHook preBuild
              export HOME=$TMPDIR
              export DENO_DIR=$out/deno-dir
              mkdir -p "$DENO_DIR"
              # --frozen refuses a lockfile drift instead of resolving anew.
              deno cache --frozen $(find src tests scripts -name '*.ts' -type f)
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              runHook postInstall
            '';
          };

          kanbei = pkgs.stdenvNoCC.mkDerivation {
            pname = "kanbei";
            version = "0.1.0";
            src = self;
            nativeBuildInputs = [
              pkgs.age
              pkgs.deno
              pkgs.makeWrapper
            ];
            doCheck = true;
            buildPhase = ''
              runHook preBuild
              export HOME=$TMPDIR
              export XDG_CACHE_HOME=$TMPDIR/cache
              export DENO_DIR=${denoDeps}/deno-dir
              deno check --no-remote src/main.ts src/journey/cli.ts src/cli/evidence.ts
              runHook postBuild
            '';
            checkPhase = ''
              runHook preCheck
              export HOME=$TMPDIR
              export XDG_CACHE_HOME=$TMPDIR/cache
              export DENO_DIR=${denoDeps}/deno-dir
              deno task test:unit
              runHook postCheck
            '';
            installPhase = ''
              runHook preInstall
              mkdir -p $out/lib/kanbei $out/bin
              cp -r src deno.json deno.lock $out/lib/kanbei/
              makeWrapper ${pkgs.deno}/bin/deno $out/bin/kanbei \
                --add-flags "run --no-remote --allow-read --allow-env --allow-net --allow-run --allow-sys" \
                --add-flags "$out/lib/kanbei/src/main.ts" \
                --set DENO_DIR ${denoDeps}/deno-dir
              runHook postInstall
            '';
            meta = {
              description = "Kanbei control plane (NIC-less VM kernel, six channels, evidence harness)";
              platforms = systems;
            };
          };
        in
        {
          inherit denoDeps;
          default = kanbei;
        }
      );

      checks = forAllSystems (pkgs: {
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);
    };
}
