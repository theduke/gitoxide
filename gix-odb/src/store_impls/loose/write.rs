use std::{convert::TryInto, fs, io, io::Write, path::PathBuf};

use gix_features::{hash, zlib::stream::deflate};
use gix_object::WriteTo;
use tempfile::NamedTempFile;

use super::Store;
use crate::store_impls::loose;

/// Returned by the [`crate::Write`] trait implementation of [`Store`]
#[derive(thiserror::Error, Debug)]
#[allow(missing_docs)]
pub enum Error {
    #[error("Could not {message} '{path}'")]
    Io {
        source: io::Error,
        message: &'static str,
        path: PathBuf,
    },
    #[error("An IO error occurred while writing an object")]
    IoRaw(#[from] io::Error),
    #[error("Could not turn temporary file into persisted file at '{target}'")]
    Persist {
        source: tempfile::PersistError,
        target: PathBuf,
    },
}

impl crate::traits::Write for Store {
    type Error = Error;

    fn write(&self, object: impl WriteTo) -> Result<gix_hash::ObjectId, Self::Error> {
        let mut to = self.dest()?;
        to.write_all(&object.loose_header()).map_err(|err| Error::Io {
            source: err,
            message: "write header to tempfile in",
            path: self.path.to_owned(),
        })?;
        object.write_to(&mut to).map_err(|err| Error::Io {
            source: err,
            message: "stream all data into tempfile in",
            path: self.path.to_owned(),
        })?;
        to.flush()?;
        self.finalize_object(to)
    }

    /// Write the given buffer in `from` to disk in one syscall at best.
    ///
    /// This will cost at least 4 IO operations.
    fn write_buf(&self, kind: gix_object::Kind, from: &[u8]) -> Result<gix_hash::ObjectId, Self::Error> {
        let mut to = self.dest()?;
        to.write_all(&gix_object::encode::loose_header(kind, from.len()))
            .map_err(|err| Error::Io {
                source: err,
                message: "write header to tempfile in",
                path: self.path.to_owned(),
            })?;

        to.write_all(from).map_err(|err| Error::Io {
            source: err,
            message: "stream all data into tempfile in",
            path: self.path.to_owned(),
        })?;
        to.flush()?;
        self.finalize_object(to)
    }

    /// Write the given stream in `from` to disk with at least one syscall.
    ///
    /// This will cost at least 4 IO operations.
    fn write_stream(
        &self,
        kind: gix_object::Kind,
        size: u64,
        mut from: impl io::Read,
    ) -> Result<gix_hash::ObjectId, Self::Error> {
        let mut to = self.dest()?;
        to.write_all(&gix_object::encode::loose_header(
            kind,
            size.try_into().expect("object size to fit into usize"),
        ))
        .map_err(|err| Error::Io {
            source: err,
            message: "write header to tempfile in",
            path: self.path.to_owned(),
        })?;

        io::copy(&mut from, &mut to).map_err(|err| Error::Io {
            source: err,
            message: "stream all data into tempfile in",
            path: self.path.to_owned(),
        })?;
        to.flush()?;
        self.finalize_object(to)
    }
}

type CompressedTempfile = deflate::Write<NamedTempFile>;

impl Store {
    fn dest(&self) -> Result<hash::Write<CompressedTempfile>, Error> {
        Ok(hash::Write::new(
            deflate::Write::new(NamedTempFile::new_in(&self.path).map_err(|err| Error::Io {
                source: err,
                message: "create named temp file in",
                path: self.path.to_owned(),
            })?),
            self.object_hash,
        ))
    }

    fn finalize_object(
        &self,
        hash::Write { hash, inner: file }: hash::Write<CompressedTempfile>,
    ) -> Result<gix_hash::ObjectId, Error> {
        let id = gix_hash::ObjectId::from(hash.digest());
        let object_path = loose::hash_path(&id, self.path.clone());
        let object_dir = object_path
            .parent()
            .expect("each object path has a 1 hex-bytes directory");
        if let Err(err) = fs::create_dir(object_dir) {
            match err.kind() {
                io::ErrorKind::AlreadyExists => {}
                _ => return Err(err.into()),
            }
        }
        let file = file.into_inner();
        file.persist(&object_path).map_err(|err| Error::Persist {
            source: err,
            target: object_path,
        })?;
        Ok(id)
    }
}
