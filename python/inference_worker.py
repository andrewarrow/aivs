"""Run one neural-model request and exit, releasing its model resources."""
from contextlib import redirect_stdout
import json
import sys
import traceback

VERSION = 1
PREFIX = "AIVS_INFERENCE "


def main():
    protocol = sys.stdout

    def send(**record):
        print(PREFIX + json.dumps({"version": VERSION, **record}, ensure_ascii=False, allow_nan=False),
              file=protocol, flush=True)

    try:
        request = json.load(sys.stdin)
        if request.get("version") != VERSION:
            raise ValueError("Unsupported Python inference protocol version.")
        operation, args = request["operation"], request["args"]
        with redirect_stdout(sys.stderr):
            if operation == "json_digest":
                import hashlib
                options = {"sort_keys": True, "ensure_ascii": args.get("ascii", False), "allow_nan": False}
                if args.get("compact", True):
                    options["separators"] = (",", ":")
                result = hashlib.sha256(json.dumps(args["value"], **options).encode()).hexdigest()
            elif operation == "token_count":
                from huggingface_hub import hf_hub_download
                from tokenizers import Tokenizer
                from quality_backend import MODEL_REPO, MODEL_REVISION, CACHE
                path = hf_hub_download(MODEL_REPO, "tokenizer.json", revision=MODEL_REVISION,
                                       cache_dir=str(CACHE / "hub"), local_files_only=True, token=False)
                tokenizer = Tokenizer.from_file(path)
                result = [len(tokenizer.encode(prompt.strip()).ids) for prompt in args["prompts"]]
            elif operation in ("model_check", "model_doctor", "model_download"):
                import quality_backend
                if operation == "model_doctor":
                    quality_backend.doctor()
                    result = None
                else:
                    result = str(quality_backend.model_path(profile=args.get("profile", "hq"),
                                                            download=operation == "model_download",
                                                            on_progress=lambda **data:
                                                                send(event={"type": "progress", **data})))
                    if operation == "model_download":
                        send(event={"type": "progress", "label": "Video models downloaded", "fraction": 1.0})
            elif operation == "ltx_render":
                import quality_backend
                result = quality_backend._render_model(**args, on_progress=lambda **data:
                    send(event={"type": "inference_progress", "data": data}))
            elif operation in ("mflux_check", "mflux_generate"):
                import storyboard_mflux
                result = (storyboard_mflux._identity() if operation == "mflux_check"
                          else storyboard_mflux._generate(args))
            else:
                raise ValueError("Unknown Python inference operation.")
        send(result=result)
        return 0
    except Exception as exc:
        traceback.print_exc(file=sys.stderr)
        send(error=str(exc) or type(exc).__name__)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
