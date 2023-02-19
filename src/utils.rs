#[macro_export]
macro_rules! stderr {
    () => (io::stderr().write_all(&[0; 0]).await);
    ($($arg:tt)*) => ({
        use tokio::io::AsyncWriteExt;
        tokio::io::stderr().write_all(&std::format!($($arg)*).as_bytes()).await
    })
}

pub fn sha3_str(text: &str) -> String {
    use sha3::{Digest, Sha3_256};
    let mut hasher = Sha3_256::new();
    hasher.update(text.as_bytes());
    let result = hasher.finalize();
    let hashname = hex::encode(result);
    hashname
}
