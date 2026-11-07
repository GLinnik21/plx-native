//! Raw pointer handed to a physical feed worker while its caller retains the owning box.
/// The caller must keep the pointed-to allocation alive until the worker has returned and
/// joined. This preserves the existing demux and Load worker lifetime contract.
pub struct SendPtr<T>(pub *mut T);
unsafe impl<T> Send for SendPtr<T> {}
