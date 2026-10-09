"""Small boundary scanner for the explicitly reviewed first-batch fixtures.

This is not a Quirrel parser or a library-wide converter. It separates comments,
quoted literals and tokens, validates byte edit boundaries, and finds complete
assert(...) calls. Only the hash-pinned recipes use it; nested interpolation and
general Quirrel grammar are outside its conversion scope.
"""
from dataclasses import dataclass


@dataclass(frozen=True)
class Token:
    start: int
    end: int
    text: str
    kind: str


def tokens(source):
    offsets = [0]
    for char in source:
        offsets.append(offsets[-1] + len(char.encode("utf-8")))
    result = []
    i = 0
    while i < len(source):
        start = i
        char = source[i]
        kind = "code"
        if char.isspace():
            kind = "trivia"
            i += 1
            while i < len(source) and source[i].isspace():
                i += 1
        elif source.startswith("//", i):
            kind = "comment"
            newline = source.find("\n", i)
            i = len(source) if newline < 0 else newline
        elif source.startswith("/*", i):
            kind = "comment"
            depth = 1
            i += 2
            while i < len(source) and depth:
                if source.startswith("/*", i):
                    depth += 1
                    i += 2
                elif source.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            if depth:
                raise ValueError("unclosed comment in reviewed fixture")
        elif char in ('"', "'"):
            kind = "literal"
            quote = char
            i += 1
            while i < len(source):
                if source[i] == "\\":
                    i += 2
                elif source[i] == quote:
                    i += 1
                    break
                else:
                    i += 1
            else:
                raise ValueError("unclosed literal in reviewed fixture")
        elif char.isalnum() or char == "_":
            i += 1
            while i < len(source) and (source[i].isalnum() or source[i] == "_"):
                i += 1
        else:
            operators = ("...", ">>>", "<=>", "..=", "==", "!=", "<=", ">=", "&&",
                         "||", "??", "+=", "-=", "*=", "/=", "++", "--", "<-", "..")
            op = next((op for op in operators if source.startswith(op, i)), char)
            i += len(op)
        result.append(Token(offsets[start], offsets[i], source[start:i], kind))
    return result


def check_boundary(stream, offset):
    for token in stream:
        if token.kind != "trivia" and token.start < offset < token.end:
            raise ValueError(f"edit cuts {token.kind} token {token.text!r} at byte {offset}")


def assertions(source):
    stream = [token for token in tokens(source) if token.kind not in ("trivia", "comment")]
    found = []
    for i, token in enumerate(stream):
        if token.text != "assert" or i + 1 >= len(stream) or stream[i + 1].text != "(":
            continue
        depth = 0
        for j in range(i + 1, len(stream)):
            if stream[j].text == "(":
                depth += 1
            elif stream[j].text == ")":
                depth -= 1
                if depth == 0:
                    found.append({"range": [token.start, stream[j].end],
                                  "tokens": [part.text for part in stream[i:j + 1]]})
                    break
        else:
            raise ValueError("unclosed assert call")
    return found
