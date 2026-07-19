use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct RepaintNotifier {
    callback: Arc<dyn Fn() + Send + Sync>,
}

impl RepaintNotifier {
    pub(crate) fn new(callback: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            callback: Arc::new(callback),
        }
    }

    pub(crate) fn request_repaint(&self) {
        (self.callback)();
    }
}
