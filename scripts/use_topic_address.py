"""Replace Address::from_slice(&expr[12..]) / [12..32] with topic_address."""
from __future__ import annotations

import re
from pathlib import Path

PAT = re.compile(
    r"Address::from_slice\(&(?P<expr>[^;\n]+?)\[12\.\.(?:32)?\]\)"
)


def add_import(text: str) -> str:
    if "topic_address" not in text.split("fn ", 1)[0] and "use crate::utils::topic_address" not in text:
        if "use crate::utils::abi_decode_address;" in text:
            text = text.replace(
                "use crate::utils::abi_decode_address;",
                "use crate::utils::{abi_decode_address, topic_address};",
            )
        elif "use crate::utils::{abi_decode_address, u128_from_be_bytes};" in text:
            text = text.replace(
                "use crate::utils::{abi_decode_address, u128_from_be_bytes};",
                "use crate::utils::{abi_decode_address, topic_address, u128_from_be_bytes};",
            )
        else:
            # insert after the first use crate line, or after module docs
            m = re.search(r"(?m)^(use .+;\n)", text)
            if m:
                text = text[: m.end()] + "use crate::utils::topic_address;\n" + text[m.end() :]
            else:
                text = "use crate::utils::topic_address;\n" + text
    elif "fn topic_address" not in text and "topic_address" in text:
        if "use crate::utils::topic_address" not in text and "topic_address," not in text:
            m = re.search(r"(?m)^(use .+;\n)", text)
            if m:
                text = text[: m.end()] + "use crate::utils::topic_address;\n" + text[m.end() :]
    return text


def main() -> None:
    for path in Path("core/src").rglob("*.rs"):
        if path.name == "utils.rs":
            continue
        text = path.read_text(encoding="utf-8")
        new, n = PAT.subn(r"topic_address(&\g<expr>)", text)
        if n:
            new = add_import(new)
            path.write_text(new, encoding="utf-8", newline="\n")
            print(f"{path}: {n}")


if __name__ == "__main__":
    main()
