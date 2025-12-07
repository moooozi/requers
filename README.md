# requers

A fast, Rust-based HTTP client for Python with a requests-like API.

## Installation

```bash
pip install requers
```

## Usage

```python
import requers

# GET request
response = requers.get('https://httpbin.org/get')
print(response.status_code)
print(response.text)

# Download a file
handle = requers.download_file('https://example.com/file.zip', 'file.zip')
while not handle.is_finished():
    progress = handle.get_progress()
    print(f"Downloaded: {progress['downloaded']} / {progress['total']}")
```

## Features

- Fast HTTP requests powered by Rust
- File downloading with progress tracking and resumability
- SHA256 hashing for files and bytes
- Similar API to the popular `requests` library

## License

MIT