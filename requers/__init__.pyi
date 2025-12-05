from typing import Any

class Response:
    status_code: int
    text: str
    headers: dict[str, str]
    url: str
    ok: bool

    def raise_for_status(self) -> None: ...
    def json(self) -> Any: ...

class DownloadHandle:
    def cancel(self) -> None: ...
    def abort(self) -> None: ...
    def get_progress(self) -> dict[str, Any]: ...
    def is_finished(self) -> bool: ...
    def is_successful(self) -> bool: ...

def get(
    url: str,
    headers: dict[str, str] | None = None,
    params: dict[str, str] | None = None,
    timeout: float | None = None,
    allow_redirects: bool = True,
    proxies: dict[str, str] | None = None,
    auth: dict[str, str] | None = None,
) -> Response: ...
def download_file(
    url: str,
    path: str,
    resume: bool = True,
    headers: dict[str, str] | None = None,
    buffer_size: int = 65536,
) -> DownloadHandle: ...
def hash_file(path: str) -> str: ...
def hash_bytes(data: bytes) -> str: ...
