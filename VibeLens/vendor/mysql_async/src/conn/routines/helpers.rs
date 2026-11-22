//! Private routine helpers.

use std::sync::Arc;

use mysql_common::{
    constants::MAX_PAYLOAD_LEN,
    io::{ParseBuf, ReadMysqlExt},
    packets::{ComStmtSendLongData, LocalInfilePacket},
    value::Value,
};

use crate::{error::LocalInfileError, queryable::Protocol, Conn, Error, Statement};

impl Conn {
    /// Helper, that sends all `Value::Bytes` in the given list of parameters as long data.
    pub(super) async fn send_long_data<'a, I>(
        &mut self,
        statement_id: u32,
        params: I,
    ) -> crate::Result<()>
    where
        I: Iterator<Item = &'a Value>,
    {
        for (i, value) in params.enumerate() {
            if let Value::Bytes(bytes) = value {
                let chunks = bytes.chunks(MAX_PAYLOAD_LEN - 6);
                let chunks = chunks.chain(if bytes.is_empty() {
                    Some(&[][..])
                } else {
                    None
                });
                for chunk in chunks {
                    let com = ComStmtSendLongData::new(statement_id, i as u16, chunk);
                    self.write_command(&com).await?;
                }
            }
        }

        Ok(())
    }

    /// Will read result set and write pending result into `self` (if any).
    pub(super) async fn read_result_set<P>(
        &mut self,
        is_first_result_set: bool,
        stmt: Option<&Statement>,
    ) -> crate::Result<()>
    where
        P: Protocol,
    {
        let packet = match self.read_packet().await {
            Ok(packet) => packet,
            Err(err @ Error::Server(_)) if is_first_result_set => {
                // shortcut to emit an error right to the caller of a query/execute
                return Err(err);
            }
            Err(Error::Server(error)) => {
                // error will be consumed as a part of a multi-result set
                self.set_pending_result_error(error)?;
                return Ok(());
            }
            Err(err) => {
                // non-server errors are fatal
                return Err(err);
            }
        };

        match packet.first() {
            Some(0x00) => {
                self.set_pending_result(Some(P::result_set_meta(Arc::from(
                    Vec::new().into_boxed_slice(),
                ))))?;
            }
            Some(0xFB) => self.handle_local_infile::<P>(&packet).await?,
            _ => self.handle_result_set::<P>(&packet, stmt).await?,
        }

        Ok(())
    }

    /// Will handle local infile packet.
    ///
    /// VibeLens patch: a server asking for a local file (`LOCAL INFILE` request, 0xFB) never
    /// gets one, whatever handler is registered: no handler is consulted and no file is opened.
    /// `CLIENT_LOCAL_FILES` is not advertised (see `Opts::get_capabilities`), so only a hostile
    /// or broken server sends this request. The connection is closed instead of answered, so
    /// nothing more is read from or sent to that server on it. See vendor/mysql_async/PATCH.md.
    pub(super) async fn handle_local_infile<P>(&mut self, packet: &[u8]) -> crate::Result<()>
    where
        P: Protocol,
    {
        let _ = ParseBuf(packet).parse::<LocalInfilePacket>(())?;
        self.inner.infile_handler = None;
        self.inner.stream.take();
        self.inner.disconnected = true;
        Err(LocalInfileError::NoHandler.into())
    }

    /// Helper that handles result set packet.
    ///
    /// Requires that `packet` contains non-zero length-encoded integer.
    pub(super) async fn handle_result_set<P>(
        &mut self,
        mut packet: &[u8],
        stmt: Option<&Statement>,
    ) -> crate::Result<()>
    where
        P: Protocol,
    {
        let column_count = packet.read_lenenc_int()?;
        // VibeLens patch: the count comes from the server; cap it before it sizes anything.
        if column_count == 0 || column_count > crate::conn::MAX_COLUMNS as u64 {
            return Err(crate::DriverError::UnexpectedPacket {
                payload: Vec::new(),
            }
            .into());
        }
        // Reading 1st byte only if metadata skip is not possible, otherwise it can be a part of the first column definition.
        // Reading column metadata only if it is not skipped - if skip is not possible or if metadata is present.
        let meta = if !(P::metadata_skip_possible(self) && packet.first() == Some(&0x00)) {
            let columns = self.read_column_defs(column_count as usize).await?;
            // Statement must have latest version of the metadata.
            if let Some(stmt) = stmt {
                stmt.update_columns_metadata(columns.clone());
            }
            P::result_set_meta(Arc::from(columns.into_boxed_slice()))
        } else {
            // Since metadata is skipped, stmt must be present.
            P::result_set_meta(stmt.unwrap().columns())
        };
        self.set_pending_result(Some(meta))?;
        Ok(())
    }
}
