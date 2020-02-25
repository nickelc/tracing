//! Generic wrapper type and accompanying traits for instrumenting a single error type e.g. an
//! error enum.
//!
//! Note: This type avoids heap allocations and implements `From<E: Error>` where as the boxed
//! variant does not, so it is more convenient to work with and more efficient when working with a
//! single known error type.

use crate::{Erased, SpanTrace};
use std::error::Error;
use std::fmt::{self, Debug, Display};

/// A wrapper type for `Error`s that bundles a `SpanTrace` with an inner `Error` type.
pub struct TracedError<E> {
    inner: ErrorImpl<E>,
}

impl<E> From<E> for TracedError<E>
where
    E: Error + Send + Sync + 'static,
{
    fn from(error: E) -> Self {
        // # SAFETY
        //
        // This function + the repr(C) on the ErrorImpl make the type erasure throughout the rest
        // of this struct's methods safe. This saves a function pointer that is parameterized on the Error type
        // being stored inside the ErrorImpl. This lets the object_ref function safely cast a type
        // erased `ErrorImpl` back to its original type, which is needed in order to forward our
        // error/display/debug impls to the internal error type from the type erased error type.
        //
        // The repr(C) is necessary to ensure that the struct is layed out in the order we
        // specified it so that we can safely access the vtable and spantrace fields thru a type
        // erased pointer to the original object.
        let vtable = &ErrorVTable {
            object_ref: object_ref::<E>,
        };

        Self {
            inner: ErrorImpl {
                vtable,
                span_trace: SpanTrace::capture(),
                error,
            },
        }
    }
}

#[repr(C)]
pub(crate) struct ErrorImpl<E> {
    vtable: &'static ErrorVTable,
    pub(crate) span_trace: SpanTrace,
    // NOTE: Don't use directly. Use only through vtable. Erased type may have
    // different alignment.
    error: E,
}

impl ErrorImpl<Erased> {
    pub(crate) fn error(&self) -> &(dyn Error + Send + Sync + 'static) {
        // # SAFETY
        //
        // this function is used to cast a type-erased pointer to a pointer to error's
        // original type. the `ErrorImpl::error` method, which calls this function, requires that
        // the type this function casts to be the original erased type of the error; failure to
        // uphold this is UB. since the `From` impl is parameterized over the original error type,
        // the function pointer we construct here will also retain the original type. therefore,
        // when this is consumed by the `error` method, it will be safe to call.
        unsafe { &*(self.vtable.object_ref)(self) }
    }
}

struct ErrorVTable {
    object_ref: unsafe fn(&ErrorImpl<Erased>) -> &(dyn Error + Send + Sync + 'static),
}

// # SAFETY
//
// This function must be parameterized on the type E of the original error that is being stored
// inside of the `ErrorImpl`. When it is parameterized by the correct type, it safely
// casts the erased `ErrorImpl` pointer type back to the original pointer type.
unsafe fn object_ref<E>(e: &ErrorImpl<Erased>) -> &(dyn Error + Send + Sync + 'static)
where
    E: Error + Send + Sync + 'static,
{
    // Attach E's native Error vtable onto a pointer to e.error.
    &(*(e as *const ErrorImpl<Erased> as *const ErrorImpl<E>)).error
}

impl<E> Error for TracedError<E>
where
    E: std::error::Error + 'static,
{
    // # SAFETY
    //
    // This function is safe so long as all functions on `ErrorImpl<Erased>` uphold the invariant
    // that the wrapped error is only ever accessed by the `error` method. This method uses the
    // function in the vtable to safely convert the pointer type back to the original type, and
    // then returns the reference to the erased error.
    //
    // This function is necessary for the `downcast_ref` in `ExtractSpanTrace` to work, because it
    // needs a concrete type to downcast to and we cannot downcast to ErrorImpls parameterized on
    // errors defined in other crates. By erasing the type here we can always cast back to the
    // Erased version of the ErrorImpl pointer, and still access the internal error type safely
    // through the vtable.
    fn source<'a>(&'a self) -> Option<&'a (dyn Error + 'static)> {
        let erased = unsafe { &*(&self.inner as *const ErrorImpl<E> as *const ErrorImpl<Erased>) };
        Some(erased)
    }
}

impl<E> Debug for TracedError<E>
where
    E: std::error::Error,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.inner.error, f)
    }
}

impl<E> Display for TracedError<E>
where
    E: std::error::Error,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.inner.error, f)
    }
}

impl Error for ErrorImpl<Erased> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.error().source()
    }
}

impl Debug for ErrorImpl<Erased> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("span backtrace:\n")?;
        Debug::fmt(&self.span_trace, f)
    }
}

impl Display for ErrorImpl<Erased> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("span backtrace:\n")?;
        Display::fmt(&self.span_trace, f)
    }
}

/// Extension trait for instrumenting errors with `SpanTrace`s
pub trait InstrumentError {
    /// The type of the wrapped error after instrumentation
    type Instrumented;

    /// Instrument an Error by bundling it with a SpanTrace
    ///
    /// # Examples
    ///
    /// ```rust
    /// use tracing_error::generic::{TracedError, InstrumentError};
    ///
    /// fn wrap_error<E>(e: E) -> TracedError<E>
    /// where
    ///     E: std::error::Error + Send + Sync + 'static,
    /// {
    ///     e.in_current_span()
    /// }
    /// ```
    fn in_current_span(self) -> Self::Instrumented;
}

/// Extension trait for instrumenting errors in `Result`s with `SpanTrace`s
pub trait InstrumentResult<T> {
    /// The type of the wrapped error after instrumentation
    type Instrumented;

    /// Instrument an Error by bundling it with a SpanTrace
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use std::{io, fs};
    /// use tracing_error::generic::{TracedError, InstrumentResult};
    ///
    /// # fn fallible_fn() -> io::Result<()> { fs::read_dir("......").map(drop) };
    ///
    /// fn do_thing() -> Result<(), TracedError<io::Error>> {
    ///     fallible_fn().in_current_span()
    /// }
    /// ```
    fn in_current_span(self) -> Result<T, Self::Instrumented>;
}

impl<T, E> InstrumentResult<T> for Result<T, E>
where
    E: InstrumentError,
{
    type Instrumented = <E as InstrumentError>::Instrumented;

    fn in_current_span(self) -> Result<T, Self::Instrumented> {
        self.map_err(E::in_current_span)
    }
}

impl<E> InstrumentError for E
where
    E: Error + Send + Sync + 'static,
{
    type Instrumented = TracedError<E>;

    fn in_current_span(self) -> Self::Instrumented {
        TracedError::from(self)
    }
}
