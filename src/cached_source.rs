use std::{
  borrow::Cow,
  hash::{BuildHasherDefault, Hash, Hasher},
  sync::{Arc, Mutex, OnceLock},
};

use dashmap::DashMap;
use rustc_hash::FxHasher;

use crate::{
  encoder::create_encoder,
  helpers::{
    stream_chunks_of_raw_source, stream_chunks_of_source_map, GeneratedInfo,
    OnChunk, OnName, OnSource, StreamChunks,
  },
  BoxSource, MapOptions, Source, SourceMap,
};

/// It tries to reused cached results from other methods to avoid calculations,
/// usually used after modify is finished.
///
/// - [webpack-sources docs](https://github.com/webpack/webpack-sources/#cachedsource).
///
/// ```
/// use rspack_sources::{
///   BoxSource, CachedSource, ConcatSource, MapOptions, OriginalSource,
///   RawSource, Source, SourceExt, SourceMap,
/// };
///
/// let mut concat = ConcatSource::new([
///   RawSource::from("Hello World\n".to_string()).boxed(),
///   OriginalSource::new(
///     "console.log('test');\nconsole.log('test2');\n",
///     "console.js",
///   )
///   .boxed(),
/// ]);
/// concat.add(OriginalSource::new("Hello2\n", "hello.md"));
///
/// let cached = CachedSource::new(concat);
///
/// assert_eq!(
///   cached.source(),
///   "Hello World\nconsole.log('test');\nconsole.log('test2');\nHello2\n"
/// );
/// // second time will be fast.
/// assert_eq!(
///   cached.source(),
///   "Hello World\nconsole.log('test');\nconsole.log('test2');\nHello2\n"
/// );
/// ```
pub struct CachedSource {
  inner: Arc<Mutex<Option<BoxSource>>>,
  cached_buffer: Arc<OnceLock<Vec<u8>>>,
  cached_source: Arc<OnceLock<Arc<str>>>,
  cached_hash: Arc<OnceLock<u64>>,
  cached_maps:
    Arc<DashMap<bool, Option<SourceMap>, BuildHasherDefault<FxHasher>>>,
}

impl CachedSource {
  /// Create a [CachedSource] with the original [Source].
  pub fn new<T: Source + 'static>(inner: T) -> Self {
    Self {
      inner: Arc::new(Mutex::new(Some(Arc::new(inner)))),
      cached_buffer: Default::default(),
      cached_source: Default::default(),
      cached_hash: Default::default(),
      cached_maps: Default::default(),
    }
  }

  fn stream_and_get_source_and_map<'a>(
    &'a self,
    input_source: &BoxSource,
    options: &MapOptions,
    on_chunk: OnChunk<'_, 'a>,
    on_source: OnSource<'_, 'a>,
    on_name: OnName<'_, 'a>,
  ) -> GeneratedInfo {
    let code = self
      .cached_source
      .get_or_init(|| input_source.source().into());
    let mut code_start = 0;
    let mut code_end = 0;

    self.cached_buffer.get_or_init(|| code.as_bytes().to_vec());

    let mut mappings_encoder = create_encoder(options.columns);
    let mut sources: Vec<String> = Vec::new();
    let mut sources_content: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();

    let generated_info = input_source.stream_chunks(
      options,
      &mut |chunk, mapping| {
        mappings_encoder.encode(&mapping);
        if let Some(chunk) = chunk {
          code_start += chunk.len();
          code_end += chunk.len();
          on_chunk(Some(Cow::Borrowed(&code[code_start..code_end])), mapping);
        } else {
          on_chunk(Some(Cow::Borrowed("")), mapping);
        }
      },
      &mut |source_index, source, source_content| {
        let source_index2 = source_index as usize;
        while sources.len() <= source_index2 {
          sources.push("".into());
        }
        sources[source_index2] = source.to_string();
        if let Some(source_content) = source_content {
          while sources_content.len() <= source_index2 {
            sources_content.push("".into());
          }
          sources_content[source_index2] = source_content.to_string();
        }
        #[allow(unsafe_code)]
        let source = unsafe {
          std::mem::transmute::<&String, &'a String>(&sources[source_index2])
        };
        #[allow(unsafe_code)]
        let source_content = unsafe {
          std::mem::transmute::<&String, &'a String>(
            &sources_content[source_index2],
          )
        };
        on_source(source_index, Cow::Borrowed(source), Some(source_content));
      },
      &mut |name_index, name| {
        let name_index2 = name_index as usize;
        while names.len() <= name_index2 {
          names.push("".into());
        }
        names[name_index2] = name.to_string();
        #[allow(unsafe_code)]
        let name = unsafe {
          std::mem::transmute::<&String, &'a String>(&names[name_index2])
        };
        on_name(name_index, Cow::Borrowed(name));
      },
    );

    let mappings = mappings_encoder.drain();
    let map = if mappings.is_empty() {
      None
    } else {
      Some(SourceMap::new(mappings, sources, sources_content, names))
    };
    self.cached_maps.insert(options.columns, map);

    generated_info
  }

  fn try_separate(&self, original: &mut Option<BoxSource>) {
    if self.cached_buffer.get().is_some()
      && self.cached_hash.get().is_some()
      && self.cached_maps.get(&true).is_some()
      && self.cached_source.get().is_some()
    {
      original.take();
    }
  }
}

impl Source for CachedSource {
  fn source(&self) -> Cow<str> {
    let cached = self.cached_source.get_or_init(|| {
      let original = self.inner.lock().unwrap();
      original.as_ref().unwrap().source().into()
    });
    Cow::Borrowed(cached)
  }

  fn buffer(&self) -> Cow<[u8]> {
    let cached = self.cached_buffer.get_or_init(|| {
      let original = self.inner.lock().unwrap();
      original.as_ref().unwrap().buffer().into()
    });
    Cow::Borrowed(cached)
  }

  fn size(&self) -> usize {
    self.source().len()
  }

  fn map(&self, options: &MapOptions) -> Option<SourceMap> {
    if let Some(map) = self.cached_maps.get(&options.columns) {
      map.clone()
    } else {
      let original = self.inner.lock().unwrap();
      let map = original.as_ref().unwrap().map(options);
      self.cached_maps.insert(options.columns, map.clone());
      map
    }
  }
}

impl StreamChunks<'_> for CachedSource {
  fn stream_chunks<'a>(
    &'a self,
    options: &MapOptions,
    on_chunk: crate::helpers::OnChunk<'_, 'a>,
    on_source: crate::helpers::OnSource<'_, 'a>,
    on_name: crate::helpers::OnName<'_, 'a>,
  ) -> crate::helpers::GeneratedInfo {
    if let Some(cache) = self.cached_maps.get(&options.columns) {
      let source = self.cached_source.get_or_init(|| {
        let original = self.inner.lock().unwrap();
        original.as_ref().unwrap().source().into()
      });
      if let Some(map) = cache.as_ref() {
        #[allow(unsafe_code)]
        // SAFETY: We guarantee that once a `SourceMap` is stored in the cache, it will never be removed.
        // Therefore, even if we force its lifetime to be longer, the reference remains valid.
        // This is based on the following assumptions:
        // 1. `SourceMap` will be valid for the entire duration of the application.
        // 2. The cached `SourceMap` will not be manually removed or replaced, ensuring the reference's safety.
        let map =
          unsafe { std::mem::transmute::<&SourceMap, &'a SourceMap>(map) };
        stream_chunks_of_source_map(
          source, map, on_chunk, on_source, on_name, options,
        )
      } else {
        stream_chunks_of_raw_source(
          source, options, on_chunk, on_source, on_name,
        )
      }
    } else {
      let mut original = self.inner.lock().unwrap();
      let generated_info = self.stream_and_get_source_and_map(
        original.as_ref().unwrap(),
        options,
        on_chunk,
        on_source,
        on_name,
      );
      self.try_separate(&mut original);
      generated_info
    }
  }
}

impl Clone for CachedSource {
  fn clone(&self) -> Self {
    Self {
      inner: self.inner.clone(),
      cached_buffer: self.cached_buffer.clone(),
      cached_source: self.cached_source.clone(),
      cached_hash: self.cached_hash.clone(),
      cached_maps: self.cached_maps.clone(),
    }
  }
}

impl Hash for CachedSource {
  fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
    let mut original = self.inner.lock().unwrap();
    let result = (self.cached_hash.get_or_init(|| {
      let mut hasher = FxHasher::default();
      original.as_ref().unwrap().hash(&mut hasher);
      hasher.finish()
    }))
    .hash(state);
    self.try_separate(&mut original);
    result
  }
}

impl PartialEq for CachedSource {
  fn eq(&self, other: &Self) -> bool {
    std::ptr::eq(self, other)
  }
}

impl Eq for CachedSource {}

impl std::fmt::Debug for CachedSource {
  fn fmt(
    &self,
    f: &mut std::fmt::Formatter<'_>,
  ) -> Result<(), std::fmt::Error> {
    f.debug_struct("CachedSource")
      .field("inner", &self.inner)
      .field("cached_buffer", &self.cached_buffer.get().is_some())
      .field("cached_source", &self.cached_source.get().is_some())
      .field("cached_maps", &(!self.cached_maps.is_empty()))
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use std::borrow::Borrow;

  use crate::{
    ConcatSource, OriginalSource, RawSource, SourceExt, SourceMapSource,
    WithoutOriginalOptions,
  };

  use super::*;

  #[test]
  fn line_number_should_not_add_one() {
    let source = ConcatSource::new([
      CachedSource::new(RawSource::from("\n")).boxed(),
      SourceMapSource::new(WithoutOriginalOptions {
        value: "\nconsole.log(1);\n".to_string(),
        name: "index.js".to_string(),
        source_map: SourceMap::new(
          ";AACA",
          vec!["index.js".into()],
          vec!["// DELETE IT\nconsole.log(1)".into()],
          vec![],
        ),
      })
      .boxed(),
    ]);
    let map = source.map(&Default::default()).unwrap();
    assert_eq!(map.mappings(), ";;AACA");
  }

  #[test]
  fn should_allow_to_store_and_share_cached_data() {
    let original = OriginalSource::new("Hello World", "test.txt");
    let source = CachedSource::new(original);
    let clone = source.clone();

    // fill up cache
    let map_options = MapOptions::default();
    source.source();
    source.buffer();
    source.size();
    source.map(&map_options);

    assert_eq!(clone.cached_source.get().unwrap().borrow(), source.source());
    assert_eq!(
      *clone.cached_buffer.get().unwrap(),
      source.buffer().to_vec()
    );
    assert_eq!(
      *clone.cached_maps.get(&map_options.columns).unwrap().value(),
      source.map(&map_options)
    );
  }

  #[test]
  fn should_return_the_correct_size_for_binary_files() {
    let source = OriginalSource::new(
      String::from_utf8(vec![0; 256]).unwrap(),
      "file.wasm",
    );
    let cached_source = CachedSource::new(source);

    assert_eq!(cached_source.size(), 256);
    assert_eq!(cached_source.size(), 256);
  }

  #[test]
  fn should_return_the_correct_size_for_cached_binary_files() {
    let source = OriginalSource::new(
      String::from_utf8(vec![0; 256]).unwrap(),
      "file.wasm",
    );
    let cached_source = CachedSource::new(source);

    cached_source.source();
    assert_eq!(cached_source.size(), 256);
    assert_eq!(cached_source.size(), 256);
  }

  #[test]
  fn should_return_the_correct_size_for_text_files() {
    let source = OriginalSource::new("TestTestTest", "file.js");
    let cached_source = CachedSource::new(source);

    assert_eq!(cached_source.size(), 12);
    assert_eq!(cached_source.size(), 12);
  }

  #[test]
  fn should_return_the_correct_size_for_cached_text_files() {
    let source = OriginalSource::new("TestTestTest", "file.js");
    let cached_source = CachedSource::new(source);

    cached_source.source();
    assert_eq!(cached_source.size(), 12);
    assert_eq!(cached_source.size(), 12);
  }

  #[test]
  fn should_produce_correct_output_for_cached_raw_source() {
    let map_options = MapOptions {
      columns: true,
      final_source: true,
    };

    let source = RawSource::from("Test\nTest\nTest\n");
    let mut on_chunk_count = 0;
    let mut on_source_count = 0;
    let mut on_name_count = 0;
    let generated_info = source.stream_chunks(
      &map_options,
      &mut |_chunk, _mapping| {
        on_chunk_count += 1;
      },
      &mut |_source_index, _source, _source_content| {
        on_source_count += 1;
      },
      &mut |_name_index, _name| {
        on_name_count += 1;
      },
    );

    let cached_source = CachedSource::new(source);
    cached_source.stream_chunks(
      &map_options,
      &mut |_chunk, _mapping| {},
      &mut |_source_index, _source, _source_content| {},
      &mut |_name_index, _name| {},
    );

    let mut cached_on_chunk_count = 0;
    let mut cached_on_source_count = 0;
    let mut cached_on_name_count = 0;
    let cached_generated_info = cached_source.stream_chunks(
      &map_options,
      &mut |_chunk, _mapping| {
        cached_on_chunk_count += 1;
      },
      &mut |_source_index, _source, _source_content| {
        cached_on_source_count += 1;
      },
      &mut |_name_index, _name| {
        cached_on_name_count += 1;
      },
    );

    assert_eq!(on_chunk_count, cached_on_chunk_count);
    assert_eq!(on_source_count, cached_on_source_count);
    assert_eq!(on_name_count, cached_on_name_count);
    assert_eq!(generated_info, cached_generated_info);
  }

  #[test]
  fn should_have_correct_buffer_if_cache_buffer_from_cache_source() {
    let buf = vec![128u8];
    let source = CachedSource::new(RawSource::from(buf.clone()));

    source.source();
    assert_eq!(source.buffer(), buf.as_slice());
  }
}
