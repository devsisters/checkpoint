use std::{collections::HashSet, future::Future, path::PathBuf};

use anyhow::Result;
use notify::{RecursiveMode, Watcher};
use stopper::Stopper;

pub struct FileWatcher<H> {
    handler: H,
    buffer: usize,
    stopper: Stopper,
    paths: HashSet<PathBuf>,
}

impl<H> FileWatcher<H> {
    pub fn new(handler: H, buffer: usize, stopper: Stopper) -> Self {
        Self {
            handler,
            buffer,
            stopper,
            paths: Default::default(),
        }
    }

    pub fn watch(&mut self, path: PathBuf) {
        self.paths.insert(path);
    }
}

impl<H, F> FileWatcher<H>
where
    H: Fn(notify::Event) -> F + Send + Sync + 'static,
    F: Future + Send,
{
    pub fn spawn(self) -> Result<()> {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(self.buffer);

        let mut watcher = notify::recommended_watcher(move |event_res| {
            let _ = sender.blocking_send(event_res);
        })?;
        for path in self.paths {
            watcher.watch(&path, RecursiveMode::NonRecursive)?;
        }

        tokio::spawn(async move {
            while let Some(Some(event_res)) = self.stopper.stop_future(receiver.recv()).await {
                match event_res {
                    Ok(event) => {
                        if event.kind.is_remove() {
                            for path in &event.paths {
                                let res = watcher.watch(path, RecursiveMode::NonRecursive);
                                if let Err(error) = res {
                                    tracing::error!(%error, path = %path.display(), "Failed to re-watch file");
                                }
                            }
                        }
                        (self.handler)(event).await;
                    }
                    Err(error) => {
                        tracing::error!(%error, "Failed to watch files");
                    }
                }
            }
        });

        Ok(())
    }
}
