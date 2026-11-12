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
              # Evidence runs write under the repo tree; check against a copy.
              cp -r . $TMPDIR/work
              cd $TMPDIR/work
              deno task test:unit
              deno task typecheck
              deno task journey:check
              deno task redteam:check
              runHook postCheck
            '';
            installPhase = ''
              runHook preInstall
              mkdir -p $out/lib/kanbei $out/bin
              cp -r src deno.json deno.lock $out/lib/kanbei/
              # Read roots are the deployed guest mounts (nix-config
              # modules/nixos/kanbei-vm.nix): inbox and vault read-only virtiofs,
              # the root-only secrets share, /etc/kanbei, the store, plus the
              # writable roots (reading them is required to open a file for write).
              makeWrapper ${pkgs.deno}/bin/deno $out/bin/kanbei \
                --add-flags "run --no-remote" \
                --add-flags "--allow-read=/mnt/kanbei/inbox,/mnt/kanbei/vault,/run/kanbei-secrets,/etc/kanbei,/nix/store,/mnt/kanbei/todo,/var/lib/kanbei,/run/kanbei" \
                --add-flags "--allow-write=/mnt/kanbei/todo,/var/lib/kanbei,/run/kanbei" \
                --add-flags "--allow-run=age" \
                --add-flags "--allow-net=127.0.0.1" \
                --add-flags "--allow-env=KANBEI_*" \
                --add-flags "$out/lib/kanbei/src/main.ts" \
                --set DENO_DIR ${denoDeps}/deno-dir \
                --prefix PATH : ${pkgs.age}/bin
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

      checks = forAllSystems (
        pkgs:
        let
          kanbei = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        in
        {
          default = kanbei;
          # The kernel shells out to age; a missing grant or PATH entry is only
          # observable by running the packaged wrapper. Ciphertext goes on stdin
          # because the wrapper's read roots are the VM mounts, not $TMPDIR.
          age-decrypt-smoke = pkgs.runCommand "kanbei-age-decrypt-smoke" {
            nativeBuildInputs = [ pkgs.age ];
          } ''
            export HOME=$TMPDIR
            age-keygen -o $TMPDIR/identity.txt 2>/dev/null
            recipient=$(age-keygen -y $TMPDIR/identity.txt)
            printf '{"smoke":true,"v":1}' | age -r "$recipient" -o $TMPDIR/batch.age
            got=$(${kanbei}/bin/kanbei decrypt-smoke --identity $TMPDIR/identity.txt < $TMPDIR/batch.age)
            test "$got" = '{"smoke":true,"v":1}'
            touch $out
          '';
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);
    };
}
