"""
requers - A fast, Rust-based HTTP client for Python.

A high-performance HTTP client library that provides a requests-like API
for making HTTP requests and downloading files. Built with Rust for speed
and reliability, offering features like resumable downloads, progress callbacks,
and native SSL support.
"""

from collections.abc import Callable
from typing import Any

class Response:
    """HTTP Response object similar to requests.Response."""

    status_code: int
    ok: bool
    url: str

    def raise_for_status(self) -> None:
        """Raise an exception if the response status is an error (4xx or 5xx)."""

    def json(self) -> Any:
        """Parse the response body as JSON."""

def get(
    url: str,
    headers: dict[str, str] | None = None,
    timeout: float | None = None,
) -> Response:
    """Make an HTTP GET request."""

def download_file(
    url: str,
    path: str,
    progress_callback: Callable[[dict[str, Any]], None] | None = None,
    resume: bool = True,
    callback_interval: float = 1.0,
) -> None:
    """Download a file from URL to path with optional progress callback."""

def cancel_download() -> None:
    """Cancel the current download operation."""
