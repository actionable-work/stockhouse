# Module story for the uploader's protocol selection and transfer limit.
args: let
  m = import ./lib/module.nix args;
  inherit (m) lib evaluate base hasFailure;
  withUpload = uploader: evaluate m.default (lib.recursiveUpdate base {uploader = {backend = "server";} // uploader;});
  configFile = output:
    lib.last (lib.splitString " " output.systemd.services.stockhouse-uploader.serviceConfig.ExecStart);
  explicit = configFile (withUpload {
    protocol = "v2";
    maxInflightNars = 16;
  });
  v1 = configFile (withUpload {
    protocol = "v1";
    maxInflightNars = 1;
  });
  uploaderFile = configFile (withUpload {});
in {
  module-upload-protocol = assert hasFailure (withUpload {maxInflightNars = 0;}) "maxInflightNars";
  assert hasFailure (withUpload {maxInflightNars = 17;}) "maxInflightNars";
  assert !(builtins.tryEval (withUpload {protocol = "v3";}).services.stockhouse.uploader.protocol).success;
    m.check "module-upload-protocol" {
      passthru.bloomery = [
        "CLIENT-INTEGRATION-MODULE-027"
        "CLIENT-INTEGRATION-MODULE-028"
      ];
      script = ''
        grep -qx 'protocol = "auto"' ${uploaderFile}
        grep -qx 'max_inflight_nars = 4' ${uploaderFile}
        grep -qx 'protocol = "v2"' ${explicit}
        grep -qx 'max_inflight_nars = 16' ${explicit}
        grep -qx 'protocol = "v1"' ${v1}
        grep -qx 'max_inflight_nars = 1' ${v1}
      '';
    };
}
