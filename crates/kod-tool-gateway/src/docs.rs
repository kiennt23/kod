//! The docs adapter: read one approved document by its pinned ID.
use anyhow::{Context, Result};
use std::{collections::HashMap, sync::Mutex};

/// A document and its data class (design: "Data classes").
#[derive(Clone, Debug, PartialEq)]
pub struct Doc {
    pub text: String,
    pub class: String,
}

pub trait Docs: Send + Sync {
    fn read(&self, id: &str) -> Result<Doc>;
}

/// In-memory docs for tests.
#[derive(Default)]
pub struct FakeDocs(Mutex<HashMap<String, Doc>>);

impl FakeDocs {
    /// A `placeholder` doc.
    pub fn add(&self, id: &str, text: &str) {
        self.add_classed(id, text, "placeholder");
    }

    pub fn add_classed(&self, id: &str, text: &str, class: &str) {
        let mut docs = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let doc = Doc {
            text: text.to_owned(),
            class: class.to_owned(),
        };
        docs.insert(id.to_owned(), doc);
    }
}

impl Docs for FakeDocs {
    fn read(&self, id: &str) -> Result<Doc> {
        let docs = self.0.lock().unwrap_or_else(|e| e.into_inner());
        docs.get(id)
            .cloned()
            .with_context(|| format!("no doc {id}"))
    }
}
