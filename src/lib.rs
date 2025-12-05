use base64::prelude::*;
use hex;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use ureq;
use ureq::tls::{TlsConfig, TlsProvider};
use url::form_urlencoded;

/// Download handle for managing individual downloads
/// Allows cancelling, aborting, and monitoring progress of a specific download
///
/// Two cancellation methods:
/// - cancel(): Keeps partial file for resuming later
/// - abort(): Deletes partial file, cannot resume
#[pyclass]
#[derive(Clone)]
struct DownloadHandle {
    /// Cancellation flag for this download (keeps .part file for resuming)
    is_cancelled: Arc<AtomicBool>,
    /// Abort flag for this download (deletes .part file)
    is_aborted: Arc<AtomicBool>,
    /// Bytes downloaded so far
    downloaded: Arc<AtomicU64>,
    /// Total bytes to download (0 if unknown)
    total: Arc<AtomicU64>,
    /// Current download speed in bytes/sec
    speed: Arc<AtomicU64>,
    /// Estimated time remaining in seconds
    eta: Arc<AtomicU64>,
    /// Whether the download is complete
    is_finished: Arc<AtomicBool>,
    /// Whether the download was successful
    is_successful: Arc<AtomicBool>,
}

#[pymethods]
impl DownloadHandle {
    /// Cancel this download (keeps partial file for resuming later)
    fn cancel(&self) {
        self.is_cancelled.store(true, Ordering::SeqCst);
    }

    /// Abort this download (deletes partial file, cannot resume)
    fn abort(&self) {
        self.is_aborted.store(true, Ordering::SeqCst);
    }

    /// Get current progress information
    /// Returns a dict with 'downloaded', 'total', 'speed', 'eta', 'finished', 'successful'
    fn get_progress(&self, py: Python) -> PyResult<Py<PyAny>> {
        let progress_dict = PyDict::new(py);
        progress_dict.set_item("downloaded", self.downloaded.load(Ordering::SeqCst))?;
        progress_dict.set_item("total", self.total.load(Ordering::SeqCst))?;
        progress_dict.set_item("speed", self.speed.load(Ordering::SeqCst) as f64)?;
        progress_dict.set_item("eta", self.eta.load(Ordering::SeqCst) as f64)?;
        progress_dict.set_item("finished", self.is_finished.load(Ordering::SeqCst))?;
        progress_dict.set_item("successful", self.is_successful.load(Ordering::SeqCst))?;
        Ok(progress_dict.into())
    }

    /// Check if download is finished
    fn is_finished(&self) -> bool {
        self.is_finished.load(Ordering::SeqCst)
    }

    /// Check if download was successful
    fn is_successful(&self) -> bool {
        self.is_successful.load(Ordering::SeqCst)
    }
}

/// Response object similar to requests.Response
/// Represents an HTTP response with status, headers, body, and URL
#[pyclass]
struct Response {
    #[pyo3(get)]
    status_code: i32,
    #[pyo3(get)]
    text: String,
    #[pyo3(get)]
    headers: Py<PyDict>,
    #[pyo3(get)]
    url: String,
    #[pyo3(get)]
    ok: bool,
}

#[pymethods]
impl Response {
    /// Raise an exception for bad status codes (4xx or 5xx)
    /// Similar to requests.Response.raise_for_status()
    fn raise_for_status(&self) -> PyResult<()> {
        if !self.ok {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                "{} {}",
                self.status_code, self.text
            )));
        }
        Ok(())
    }

    /// Parse the response body as JSON
    /// Returns a Python object parsed from the JSON text
    fn json(&self, py: Python) -> PyResult<Py<PyAny>> {
        let json_mod = py.import("json")?;
        let data = json_mod.call_method1("loads", (&self.text,))?.unbind();
        Ok(data)
    }
}

/// Send an HTTP GET request and return a Response object
/// Similar to requests.get(), supports headers, params, timeout, redirects, proxies, and auth
///
/// # Arguments
/// * `url` - The URL to request
/// * `headers` - Optional dict of HTTP headers
/// * `params` - Optional dict of query parameters to append to URL
/// * `timeout` - Optional request timeout in seconds
/// * `allow_redirects` - Whether to follow redirects (default true)
/// * `proxies` - Optional dict of proxy settings (currently supports one proxy)
/// * `auth` - Optional dict with 'username' and 'password' for basic auth
#[pyfunction]
#[pyo3(signature = (url, headers=None, params=None, timeout=None, allow_redirects=true, proxies=None, auth=None))]
fn get(
    py: Python,
    url: &str,
    headers: Option<&Bound<'_, PyDict>>,
    params: Option<&Bound<'_, PyDict>>,
    timeout: Option<f64>,
    allow_redirects: bool,
    proxies: Option<&Bound<'_, PyDict>>,
    auth: Option<&Bound<'_, PyDict>>,
) -> PyResult<Response> {
    // Build URL with query parameters if provided
    let mut final_url = url.to_string();
    if let Some(prms) = params {
        let mut pairs = vec![];
        for (key, value) in prms.iter() {
            let key_str: String = key.extract()?;
            let value_str: String = value.extract()?;
            pairs.push((key_str, value_str));
        }
        let encoded: String = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish();
        if !encoded.is_empty() {
            final_url.push(if url.contains('?') { '&' } else { '?' });
            final_url.push_str(&encoded);
        }
    }

    // Configure HTTP agent with TLS, timeout, redirects, and proxy
    let mut config = ureq::Agent::config_builder();
    let tls_config = TlsConfig::builder()
        .provider(TlsProvider::NativeTls)
        .build();
    config = config.tls_config(tls_config);
    config = config.timeout_global(timeout.map(|t| Duration::from_secs_f64(t)));
    if !allow_redirects {
        config = config.max_redirects(0);
    }
    if let Some(prxs) = proxies {
        // Currently supports a single proxy (takes the first one)
        if let Some((_scheme, proxy_url)) = prxs.iter().next() {
            let proxy_url_str: String = proxy_url.extract()?;
            let proxy = ureq::Proxy::new(&proxy_url_str).map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Proxy error: {}", e))
            })?;
            config = config.proxy(Some(proxy));
        }
    }
    let agent = ureq::Agent::new_with_config(config.build());

    // Build the request with headers and authentication
    let mut request = agent.get(&final_url);

    // Add custom headers
    if let Some(hdrs) = headers {
        for (key, value) in hdrs.iter() {
            let key_str: String = key.extract()?;
            let value_str: String = value.extract()?;
            request = request.header(&key_str, &value_str);
        }
    }

    // Add basic authentication if provided
    if let Some(auth_dict) = auth {
        if let (Ok(Some(username)), Ok(Some(password))) = (
            auth_dict.get_item("username"),
            auth_dict.get_item("password"),
        ) {
            let username_str: String = username.extract()?;
            let password_str: String = password.extract()?;
            request = request.header(
                "Authorization",
                &format!(
                    "Basic {}",
                    BASE64_STANDARD.encode(format!("{}:{}", username_str, password_str))
                ),
            );
        }
    }

    // Send the request (releases GIL during network I/O)
    let mut response = py.detach(|| request.call()).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Request failed: {}", e))
    })?;

    // Extract response status code
    let status_code = response.status().as_u16() as i32;

    // Extract response headers into a Python dict
    let headers_dict = PyDict::new(py);
    for (key, value) in response.headers() {
        let key_str = key.as_str();
        if let Ok(value_str) = value.to_str() {
            headers_dict.set_item(key_str, value_str)?;
        }
    }

    // Read response body as text (releases GIL during I/O)
    let text = py
        .detach(|| response.body_mut().read_to_string())
        .map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                "Text decoding failed: {}",
                e
            ))
        })?;

    // Create and return Response object
    let response_obj = Response {
        status_code,
        text,
        headers: headers_dict.into(),
        url: final_url,
        ok: status_code >= 200 && status_code < 300,
    };

    Ok(response_obj)
}

/// Download a file from URL to local path with progress reporting and cancellation support
/// Returns a DownloadHandle that can be used to monitor and cancel the download
///
/// # Arguments
/// * `url` - The URL to download from
/// * `path` - Local file path to save to
/// * `resume` - Whether to resume partial downloads (default true)
/// * `headers` - Optional dict of HTTP headers
/// * `buffer_size` - Buffer size for reading chunks (default 65536)
#[pyfunction]
#[pyo3(signature = (url, path, resume=true, headers=None, buffer_size=65536))]
fn download_file(
    url: &str,
    path: &str,
    resume: bool,
    headers: Option<&Bound<'_, PyDict>>,
    buffer_size: usize,
) -> PyResult<DownloadHandle> {
    // Create download handle with shared state
    let handle = DownloadHandle {
        is_cancelled: Arc::new(AtomicBool::new(false)),
        is_aborted: Arc::new(AtomicBool::new(false)),
        downloaded: Arc::new(AtomicU64::new(0)),
        total: Arc::new(AtomicU64::new(0)),
        speed: Arc::new(AtomicU64::new(0)),
        eta: Arc::new(AtomicU64::new(0)),
        is_finished: Arc::new(AtomicBool::new(false)),
        is_successful: Arc::new(AtomicBool::new(false)),
    };

    // Clone handle for the background thread
    let handle_clone = handle.clone();
    let url = url.to_string();
    let path = path.to_string();

    // Prepare headers for the thread
    let headers_vec: Vec<(String, String)> = if let Some(hdrs) = headers {
        let mut vec = Vec::new();
        for (key, value) in hdrs.iter() {
            if let (Ok(key_str), Ok(value_str)) =
                (key.extract::<String>(), value.extract::<String>())
            {
                vec.push((key_str, value_str));
            }
        }
        vec
    } else {
        Vec::new()
    };

    // Spawn background download thread
    thread::spawn(move || {
        if let Err(_) = download_file_impl(
            &handle_clone,
            &url,
            &path,
            resume,
            &headers_vec,
            buffer_size,
        ) {
            handle_clone.is_successful.store(false, Ordering::SeqCst);
        } else {
            handle_clone.is_successful.store(true, Ordering::SeqCst);
        }
        handle_clone.is_finished.store(true, Ordering::SeqCst);
    });

    Ok(handle)
}

/// Internal download implementation
fn download_file_impl(
    handle: &DownloadHandle,
    url: &str,
    path: &str,
    resume: bool,
    headers: &[(String, String)],
    buffer_size: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    // Use temporary file for download (appends .part to path)
    let temp_path = format!("{}.part", path);
    let tls_config = TlsConfig::builder()
        .provider(TlsProvider::NativeTls)
        .build();
    let config = ureq::Agent::config_builder().tls_config(tls_config).build();
    let agent = config.new_agent();
    let mut start_byte = 0u64;

    // Check for existing partial download file if resume is enabled
    if resume {
        if let Ok(metadata) = std::fs::metadata(&temp_path) {
            start_byte = metadata.len();
        }
    }

    // Prepare HTTP request with range header for resuming
    let mut request = agent.get(url);
    if start_byte > 0 {
        request = request.header("Range", &format!("bytes={}-", start_byte));
    }

    // Add custom headers
    for (key, value) in headers {
        request = request.header(key, value);
    }

    // Send download request
    let mut response = request.call()?;

    // Determine total file size from Content-Length or Content-Range header
    let status = response.status();
    let total_size = if status == ureq::http::StatusCode::PARTIAL_CONTENT {
        response
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(|cr| cr.split('/').nth(1)?.parse::<u64>().ok())
    } else {
        response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|cl| cl.parse::<u64>().ok())
    };

    // Set total size in handle
    if let Some(total) = total_size {
        handle.total.store(total, Ordering::SeqCst);
    }

    // Initialize download tracking variables
    let mut downloaded = start_byte;
    let start_time = Instant::now();
    let mut prev_downloaded = start_byte;
    let mut prev_time = start_time;

    // Open file for writing: append if resuming, create new otherwise
    let mut file = if resume && start_byte > 0 {
        std::fs::OpenOptions::new().append(true).open(&temp_path)?
    } else {
        std::fs::File::create(&temp_path)?
    };

    use std::io::Write;

    let mut reader = response.body_mut().as_reader();

    let mut buffer = vec![0u8; buffer_size];
    loop {
        // Check if download was cancelled or aborted
        if handle.is_cancelled.load(Ordering::SeqCst) {
            return Ok(()); // Cancelled, but keep .part file for resuming
        }
        if handle.is_aborted.load(Ordering::SeqCst) {
            // Aborted - delete the partial file
            let _ = std::fs::remove_file(&temp_path);
            return Ok(());
        }

        // Read chunk from network
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break; // End of stream
        }

        // Write chunk to file
        file.write_all(&buffer[..n])?;

        downloaded += n as u64;
        handle.downloaded.store(downloaded, Ordering::SeqCst);

        // Update progress statistics
        let current_time = Instant::now();
        let elapsed_since_prev = current_time.duration_since(prev_time).as_secs_f64();
        if elapsed_since_prev >= 0.1 {
            // Update every 100ms
            let speed = if elapsed_since_prev > 0.0 {
                ((downloaded - prev_downloaded) as f64 / elapsed_since_prev) as u64
            } else {
                0
            };
            handle.speed.store(speed, Ordering::SeqCst);

            let eta = if speed > 0 && total_size.is_some() {
                ((total_size.unwrap() - downloaded) / speed) as u64
            } else {
                0
            };
            handle.eta.store(eta, Ordering::SeqCst);

            prev_downloaded = downloaded;
            prev_time = current_time;
        }
    }

    // Rename temporary file to final path on successful completion
    std::fs::rename(&temp_path, path)?;

    Ok(())
}

/// Hash a file's content using SHA256 and return the hex digest
/// Reads 1MB chunks
///
/// # Arguments
/// * `path` - Path to the file to hash
#[pyfunction]
fn hash_file(py: Python, path: &str) -> PyResult<String> {
    let mut file = std::fs::File::open(path).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyIOError, _>(format!("Failed to open file: {}", e))
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1048576]; // 1MB buffer for optimal performance with large files
    loop {
        py.check_signals()?; // Allow interruption
        // Read chunk and update hash (releases GIL during I/O)
        let n = py.detach(|| file.read(&mut buffer)).map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyIOError, _>(format!("Failed to read file: {}", e))
        })?;
        if n == 0 {
            break;
        }
        py.detach(|| hasher.update(&buffer[..n]));
    }
    let result = hasher.finalize();
    Ok(hex::encode(result))
}

/// Hash bytes using SHA256 and return the hex digest
///
/// # Arguments
/// * `data` - The bytes to hash
#[pyfunction]
fn hash_bytes(py: Python, data: &[u8]) -> PyResult<String> {
    let mut hasher = Sha256::new();
    py.detach(|| hasher.update(data)); // Release GIL during hashing
    let result = hasher.finalize();
    Ok(hex::encode(result))
}

/// Python module initialization
/// Registers the Response class, DownloadHandle class and all functions with the Python module
#[pymodule]
fn requers(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Response>()?;
    m.add_class::<DownloadHandle>()?;
    m.add_function(wrap_pyfunction!(get, m)?)?;
    m.add_function(wrap_pyfunction!(download_file, m)?)?;
    m.add_function(wrap_pyfunction!(hash_file, m)?)?;
    m.add_function(wrap_pyfunction!(hash_bytes, m)?)?;
    Ok(())
}
