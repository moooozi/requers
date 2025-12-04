use pyo3::prelude::*;
use pyo3::types::PyDict;
use reqwest::blocking::Client;
use reqwest::{Proxy, redirect::Policy};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use url::form_urlencoded;

/// Global cancellation flag for downloads
static CANCEL_FLAG: AtomicBool = AtomicBool::new(false);

/// Response object similar to requests.Response
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
    /// Raise an exception for bad status codes
    fn raise_for_status(&self) -> PyResult<()> {
        if !self.ok {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                "{} {}",
                self.status_code, self.text
            )));
        }
        Ok(())
    }

    /// Parse the response as JSON
    fn json(&self, py: Python) -> PyResult<Py<PyAny>> {
        let json_mod = py.import("json")?;
        let data = json_mod.call_method1("loads", (&self.text,))?.unbind();
        Ok(data)
    }
}

/// Send an HTTP request and return a response dict (similar to requests.Response)
#[pyfunction]
#[pyo3(signature = (url, method="GET", headers=None, params=None, timeout=30.0, allow_redirects=true, proxies=None, auth=None))]
fn get(
    py: Python,
    url: &str,
    method: &str,
    headers: Option<&Bound<'_, PyDict>>,
    params: Option<&Bound<'_, PyDict>>,
    timeout: f64,
    allow_redirects: bool,
    proxies: Option<&Bound<'_, PyDict>>,
    auth: Option<&Bound<'_, PyDict>>,
) -> PyResult<Response> {
    // Build URL with params
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

    // Build client
    let mut client_builder = Client::builder().timeout(Duration::from_secs_f64(timeout));

    // Redirects
    if !allow_redirects {
        client_builder = client_builder.redirect(Policy::none());
    }

    // Proxies
    if let Some(prxs) = proxies {
        for (scheme, proxy_url) in prxs.iter() {
            let scheme_str: String = scheme.extract()?;
            let proxy_url_str: String = proxy_url.extract()?;
            if let Ok(proxy) = Proxy::all(&proxy_url_str) {
                client_builder = client_builder.proxy(proxy);
            }
        }
    }

    let client = client_builder.build().map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Client build failed: {}", e))
    })?;

    // Build request
    let mut request = client.request(
        reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET),
        &final_url,
    );

    // Add headers
    if let Some(hdrs) = headers {
        for (key, value) in hdrs.iter() {
            let key_str: String = key.extract()?;
            let value_str: String = value.extract()?;
            request = request.header(&key_str, &value_str);
        }
    }

    // Add auth
    if let Some(auth_dict) = auth {
        if let (Ok(Some(username)), Ok(Some(password))) = (
            auth_dict.get_item("username"),
            auth_dict.get_item("password"),
        ) {
            let username_str: String = username.extract()?;
            let password_str: String = password.extract()?;
            request = request.basic_auth(username_str, Some(password_str));
        }
    }

    // Send request
    let response = request.send().map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Request failed: {}", e))
    })?;

    // Get status
    let status_code = response.status().as_u16() as i32;

    // Get final URL
    let final_url = response.url().to_string();

    // Get headers
    let headers_dict = PyDict::new(py);
    for (key, value) in response.headers() {
        let key_str = key.as_str();
        if let Ok(value_str) = value.to_str() {
            headers_dict.set_item(key_str, value_str)?;
        }
    }

    // Get text
    let text = response.text().map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Text decoding failed: {}", e))
    })?;

    // Create response object
    let response = Response {
        status_code,
        text,
        headers: headers_dict.into(),
        url: final_url,
        ok: status_code >= 200 && status_code < 300,
    };

    Ok(response)
}

/// Download a file with progress reporting, cancellation, and resume support
#[pyfunction]
#[pyo3(signature = (url, path, progress_callback=None, resume=true, callback_interval=0.0))]
fn download_file(
    py: Python,
    url: &str,
    path: &str,
    progress_callback: Option<Py<PyAny>>,
    resume: bool,
    callback_interval: f64,
) -> PyResult<()> {
    // Reset cancellation flag
    CANCEL_FLAG.store(false, Ordering::SeqCst);

    let temp_path = format!("{}.part", path);
    let client = Client::new();
    let mut start_byte = 0u64;

    // Check for existing partial file if resume is enabled
    if resume {
        if let Ok(metadata) = std::fs::metadata(&temp_path) {
            start_byte = metadata.len();
        }
    }

    // Prepare request with range header for resume
    let mut request = client.get(url);
    if start_byte > 0 {
        request = request.header("Range", format!("bytes={}-", start_byte));
    }

    let mut response = request.send().map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Request failed: {}", e))
    })?;

    let status = response.status();
    let total_size = if status == 206 {
        // Partial content, get total from Content-Range header
        response
            .headers()
            .get("content-range")
            .and_then(|cr| cr.to_str().ok())
            .and_then(|cr| cr.split('/').nth(1)?.parse::<u64>().ok())
    } else {
        response.content_length()
    };

    let mut downloaded = start_byte;
    let start_time = Instant::now();
    let mut last_callback_time = start_time;
    let mut prev_downloaded = start_byte;
    let mut prev_time = start_time;

    // Open file for writing (append if resuming)
    let mut file = if resume && start_byte > 0 {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&temp_path)
            .map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                    "Failed to open partial file for append: {}",
                    e
                ))
            })?
    } else {
        std::fs::File::create(&temp_path).map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                "Failed to create partial file: {}",
                e
            ))
        })?
    };

    use std::io::Write;

    // Read in chunks - dynamic buffer 512KB to 8MB based on speed and callback interval
    let mut buffer = vec![0u8; 524_288]; // 512KB minimum
    loop {
        let n = response.read(&mut buffer).map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Read error: {}", e))
        })?;
        if n == 0 {
            break;
        }

        // Check for cancellation
        if CANCEL_FLAG.load(Ordering::SeqCst) {
            return Ok(());
        }

        file.write_all(&buffer[..n]).map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Write error: {}", e))
        })?;

        downloaded += n as u64;

        // Report progress
        if let Some(callback) = &progress_callback {
            let current_time = Instant::now();
            let time_since_last = current_time
                .duration_since(last_callback_time)
                .as_secs_f64();
            if callback_interval <= 0.0 || time_since_last >= callback_interval {
                let elapsed_since_prev = current_time.duration_since(prev_time).as_secs_f64();
                let speed = if elapsed_since_prev > 0.0 {
                    (downloaded - prev_downloaded) as f64 / elapsed_since_prev
                } else {
                    0.0
                };
                let eta = if speed > 0.0 && total_size.is_some() {
                    ((total_size.unwrap() - downloaded) as f64) / speed
                } else {
                    0.0
                };

                // Dynamically adjust buffer size based on speed and callback interval, in 512KB steps
                let new_buffer_size = if callback_interval > 0.0 {
                    let calculated = speed * callback_interval;
                    let step = 524288.0;
                    ((calculated / step).round() * step).clamp(524288.0, 8388608.0) as usize
                } else {
                    1_048_576 // 1MB default when no interval
                };
                if buffer.len() != new_buffer_size {
                    buffer.resize(new_buffer_size, 0);
                }

                let progress_dict = PyDict::new(py);
                progress_dict.set_item("downloaded", downloaded)?;
                progress_dict.set_item("total", total_size)?;
                progress_dict.set_item("speed", speed as i64)?;
                progress_dict.set_item("eta", eta)?;

                callback.call(py, (progress_dict,), None)?;
                last_callback_time = current_time;
                prev_downloaded = downloaded;
                prev_time = current_time;
            }
        }
    }

    // On successful download, rename temp file to final path
    std::fs::rename(&temp_path, path).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to finalize download: {}",
            e
        ))
    })?;

    Ok(())
}

/// Cancel the current download
#[pyfunction]
fn cancel_download() -> PyResult<()> {
    CANCEL_FLAG.store(true, Ordering::SeqCst);
    Ok(())
}

/// A Python module implemented in Rust.
#[pymodule]
fn requers(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Response>()?;
    m.add_function(wrap_pyfunction!(get, m)?)?;
    m.add_function(wrap_pyfunction!(download_file, m)?)?;
    m.add_function(wrap_pyfunction!(cancel_download, m)?)?;
    Ok(())
}
