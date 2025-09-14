use std::{collections::HashMap, hash::Hash, sync::Arc};

use tokio::sync::{Mutex, RwLock};

/// Prevents simultaneous / rapid fire access to an action,
/// returning the cached result instead in these situations.
#[derive(Default)]
pub struct TimeoutCache<K, Res>(
  Mutex<HashMap<K, Arc<Mutex<CacheEntry<Res>>>>>,
);

impl<K: Eq + Hash, Res: Default> TimeoutCache<K, Res> {
  pub async fn get_lock(
    &self,
    key: K,
  ) -> Arc<Mutex<CacheEntry<Res>>> {
    let mut lock = self.0.lock().await;
    lock.entry(key).or_default().clone()
  }
}

pub struct CacheEntry<Res> {
  /// The last cached ts
  pub last_ts: i64,
  /// The last cached result
  pub res: anyhow::Result<Res>,
}

impl<Res: Default> Default for CacheEntry<Res> {
  fn default() -> Self {
    CacheEntry {
      last_ts: 0,
      res: Ok(Res::default()),
    }
  }
}

impl<Res: Clone> CacheEntry<Res> {
  pub fn set(&mut self, res: &anyhow::Result<Res>, timestamp: i64) {
    self.res = res.as_ref().map_err(clone_anyhow_error).cloned();
    self.last_ts = timestamp;
  }

  pub fn clone_res(&self) -> anyhow::Result<Res> {
    self.res.as_ref().map_err(clone_anyhow_error).cloned()
  }
}

fn clone_anyhow_error(e: &anyhow::Error) -> anyhow::Error {
  let mut reasons =
    e.chain().map(|e| e.to_string()).collect::<Vec<_>>();
  // Always guaranteed to be at least one reason
  // Need to start the chain with the last reason
  let mut e = anyhow::Error::msg(reasons.pop().unwrap());
  // Need to reverse reason application from lowest context to highest context.
  for reason in reasons.into_iter().rev() {
    e = e.context(reason)
  }
  e
}

pub struct CloneCache<K: PartialEq + Eq + Hash, T: Clone> {
  cache: RwLock<HashMap<K, T>>,
}

impl<K: PartialEq + Eq + Hash, T: Clone> Default
  for CloneCache<K, T>
{
  fn default() -> Self {
    Self {
      cache: RwLock::new(HashMap::new()),
    }
  }
}

impl<K: PartialEq + Eq + Hash + std::fmt::Debug + Clone, T: Clone>
  CloneCache<K, T>
{
  pub async fn get(&self, key: &K) -> Option<T> {
    self.cache.read().await.get(key).cloned()
  }

  pub async fn get_list(&self) -> Vec<T> {
    let cache = self.cache.read().await;
    cache.values().cloned().collect()
  }

  pub async fn insert<Key>(&self, key: Key, val: T) -> Option<T>
  where
    T: std::fmt::Debug,
    Key: Into<K> + std::fmt::Debug,
  {
    self.cache.write().await.insert(key.into(), val)
  }

  pub async fn remove(&self, key: &K) -> Option<T> {
    self.cache.write().await.remove(key)
  }
}

impl<
  K: PartialEq + Eq + Hash + std::fmt::Debug + Clone,
  T: Clone + Default,
> CloneCache<K, T>
{
  pub async fn get_or_insert_default(&self, key: &K) -> T {
    let mut lock = self.cache.write().await;
    match lock.get(key).cloned() {
      Some(item) => item,
      None => {
        let item: T = Default::default();
        lock.insert(key.clone(), item.clone());
        item
      }
    }
  }
}
