"""Delivery dimensions and the model's aligned generation canvas."""

_FORMATS = {
    "landscape": (1280, 720, "16:9"),
    "portrait": (1080, 1920, "9:16"),
}


def resolve_format(name: str) -> dict:
    if not isinstance(name, str) or name not in _FORMATS:
        raise ValueError("Render format must be landscape (1280×720) or portrait (1080×1920).")
    width, height, aspect = _FORMATS[name]
    return {
        "name": name, "width": width, "height": height, "aspect_ratio": aspect,
        "model_width": (width + 63) // 64 * 64,
        "model_height": (height + 63) // 64 * 64,
    }


def framing_prompt(name: str) -> str:
    if name == "portrait":
        return ("Compose for a vertical 9:16 portrait frame. Keep faces, active hands, "
                "and essential props inside the narrow central frame. ")
    return ("Compose for a horizontal 16:9 landscape frame. Keep faces, active hands, "
            "and essential props clear of the top and bottom edges. ")
