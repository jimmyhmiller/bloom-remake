//! The Vercel function (docs/design/DEPLOY.md §3): every request under `/blossom/` comes here. The deployment is
//! loaded when an instance starts; one that cannot load answers every request with why, so the deploy shows it.
// A function's standard error is its log on Vercel.
#![allow(clippy::print_stderr)]

use std::collections::BTreeMap;
use std::sync::Arc;

use blossom_vercel::{App, MAX_BODY};
use http_body_util::{BodyExt, Limited};
use vercel_runtime::{AppState, Error, Request, Response, ResponseBody, run, service_fn};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let loaded = tokio::task::spawn_blocking(App::from_env).await?;
    let app: Arc<Result<App, String>> = Arc::new(loaded);
    if let Err(why) = app.as_ref() {
        eprintln!("blossom: the deployment did not load: {why}");
    }
    run(service_fn(move |req: Request, state: AppState| {
        let app = app.clone();
        async move { handle(app, req, state).await }
    }))
    .await
}

fn text(status: u16, body: String) -> Result<Response<ResponseBody>, Error> {
    Ok(Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .header("cache-control", "no-store")
        .body(ResponseBody::from(body))?)
}

async fn handle(app: Arc<Result<App, String>>, req: Request, state: AppState) -> Result<Response<ResponseBody>, Error> {
    if let Err(why) = app.as_ref() {
        return text(500, format!("the deployment did not load: {why}"));
    }
    let (parts, body) = req.into_parts();
    let body = match Limited::new(body, MAX_BODY).collect().await {
        Ok(b) => b.to_bytes().to_vec(),
        Err(e) => return text(413, format!("the body: {e}")),
    };
    let headers: BTreeMap<String, String> = parts
        .headers
        .iter()
        .filter_map(|(k, v)| Some((k.as_str().to_ascii_lowercase(), v.to_str().ok()?.to_owned())))
        .collect();
    let request = blossom_runtime::web::Request {
        method: parts.method.as_str().to_owned(),
        path: parts.uri.path().to_owned(),
        query: parts.uri.query().unwrap_or_default().to_owned(),
        version: "HTTP/1.1".into(),
        headers,
    };
    // The engine blocks (a receive waits for the object's next commit): off the runtime's threads.
    let answering = app.clone();
    let answer = tokio::task::spawn_blocking(move || match answering.as_ref() {
        Ok(app) => Some(app.answer(&request, &body)),
        Err(_) => None,
    })
    .await?;
    let Some(answer) = answer else {
        return text(500, "the deployment did not load".into());
    };
    if let Some(fault) = &answer.fault {
        eprintln!("blossom: a request failed: {fault}");
    }
    // The store's housekeeping, after the answer is sent and at most once a minute per instance.
    if let Ok(loaded) = app.as_ref()
        && loaded.housekeeping_due()
    {
        let keeping = app.clone();
        state.wait_until(async move {
            let done = tokio::task::spawn_blocking(move || match keeping.as_ref() {
                Ok(app) => app.housekeep(),
                Err(_) => Ok(()),
            })
            .await;
            match done {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!("blossom: housekeeping: {e}"),
                Err(e) => eprintln!("blossom: housekeeping: {e}"),
            }
        });
    }
    let mut response = Response::builder()
        .status(answer.status)
        .header("content-type", answer.content_type)
        .header("cache-control", "no-store");
    for (k, v) in &answer.headers {
        response = response.header(*k, v.as_str());
    }
    Ok(response.body(ResponseBody::from(answer.body))?)
}
