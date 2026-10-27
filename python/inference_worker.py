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
            if operation == "ltx_render":
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
