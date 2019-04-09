use super::*;

use crate::frame::rtu::*;
use crate::slave::SlaveId;

use bytes::{BufMut, Bytes, BytesMut};
use log::error;
use modbus_core::{self as mb, rtu::crc16};
use std::io::{Error, ErrorKind, Result};
use tokio_codec::{Decoder, Encoder};

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct RequestDecoder;

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct ResponseDecoder;

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct ClientCodec {
    pub(crate) decoder: ResponseDecoder,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct ServerCodec {
    pub(crate) decoder: RequestDecoder,
}

impl Decoder for RequestDecoder {
    type Item = (SlaveId, Bytes);
    type Error = Error;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<(SlaveId, Bytes)>> {
        //TODO do not clone the buffer
        mb::rtu::decode(mb::rtu::DecoderType::Request, &buf.clone())
            .map(|res| {
                res.map(|(frame, location)| {
                    let mut res = Bytes::new();
                    res.extend_from_slice(frame.pdu);
                    buf.split_to(location.start + location.size);
                    (frame.slave, res)
                })
            })
            .map_err(|err| Error::new(ErrorKind::InvalidData, format!("{}", err)))
    }
}

impl Decoder for ResponseDecoder {
    type Item = (SlaveId, Bytes);
    type Error = Error;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<(SlaveId, Bytes)>> {
        //TODO do not clone the buffer
        mb::rtu::decode(mb::rtu::DecoderType::Response, &buf.clone())
            .map(|res| {
                res.map(|(frame, location)| {
                    let mut res = Bytes::new();
                    res.extend_from_slice(frame.pdu);
                    buf.split_to(location.start + location.size);
                    (frame.slave, res)
                })
            })
            .map_err(|err| Error::new(ErrorKind::InvalidData, format!("{}", err)))
    }
}

impl Decoder for ClientCodec {
    type Item = ResponseAdu;
    type Error = Error;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<ResponseAdu>> {
        self.decoder
            .decode(buf)
            .and_then(|frame| {
                if let Some((slave_id, pdu_data)) = frame {
                    let hdr = Header { slave_id };
                    // Decoding of the PDU should are unlikely to fail due
                    // to transmission errors, because the frame's bytes
                    // have already been verified with the CRC.
                    ResponsePdu::try_from(pdu_data)
                        .map(|pdu| Some(ResponseAdu { hdr, pdu }))
                        .map_err(|err| {
                            // Unrecoverable error
                            error!("Failed to decode response PDU: {}", err);
                            err
                        })
                } else {
                    Ok(None)
                }
            })
            .or_else(|_| {
                // Decoding the transport frame is non-destructive and must
                // never fail!
                unreachable!();
            })
    }
}

impl Decoder for ServerCodec {
    type Item = RequestAdu;
    type Error = Error;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<RequestAdu>> {
        self.decoder
            .decode(buf)
            .and_then(|frame| {
                if let Some((slave_id, pdu_data)) = frame {
                    let hdr = Header { slave_id };
                    // Decoding of the PDU should are unlikely to fail due
                    // to transmission errors, because the frame's bytes
                    // have already been verified with the CRC.
                    RequestPdu::try_from(pdu_data)
                        .map(|pdu| Some(RequestAdu { hdr, pdu }))
                        .map_err(|err| {
                            // Unrecoverable error
                            error!("Failed to decode request PDU: {}", err);
                            err
                        })
                } else {
                    Ok(None)
                }
            })
            .or_else(|_| {
                // Decoding the transport frame is non-destructive and must
                // never fail!
                unreachable!();
            })
    }
}

impl Encoder for ClientCodec {
    type Item = RequestAdu;
    type Error = Error;

    fn encode(&mut self, adu: RequestAdu, buf: &mut BytesMut) -> Result<()> {
        let RequestAdu { hdr, pdu } = adu;
        let pdu_data: Bytes = pdu.into();
        buf.reserve(pdu_data.len() + 3);
        buf.put_u8(hdr.slave_id);
        buf.put_slice(&*pdu_data);
        let crc = crc16(buf);
        buf.put_u16_be(crc);
        Ok(())
    }
}

impl Encoder for ServerCodec {
    type Item = ResponseAdu;
    type Error = Error;

    fn encode(&mut self, adu: ResponseAdu, buf: &mut BytesMut) -> Result<()> {
        let ResponseAdu { hdr, pdu } = adu;
        let pdu_data: Bytes = pdu.into();
        buf.reserve(pdu_data.len() + 3);
        buf.put_u8(hdr.slave_id);
        buf.put_slice(&*pdu_data);
        let crc = crc16(buf);
        buf.put_u16_be(crc);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    mod client {

        use super::*;

        #[test]
        fn decode_partly_received_client_message() {
            let mut codec = ClientCodec::default();
            let mut buf = BytesMut::from(vec![
                0x12, // slave address
                0x02, // function code
                0x03, // byte count
                0x00, // data
                0x00, // data
                0x00, // data
                0x00, // CRC first byte
                      // missing crc second byte
            ]);
            let res = codec.decode(&mut buf).unwrap();
            assert!(res.is_none());
            assert_eq!(buf.len(), 7);
        }

        #[test]
        fn decode_empty_client_message() {
            let mut codec = ClientCodec::default();
            let mut buf = BytesMut::new();
            assert_eq!(0, buf.len());

            let res = codec.decode(&mut buf).unwrap();

            assert!(res.is_none());
            assert_eq!(0, buf.len());
        }

        #[test]
        fn decode_single_byte_client_message() {
            let mut codec = ClientCodec::default();
            let mut buf = BytesMut::from(vec![0x00]);
            assert_eq!(1, buf.len());

            let res = codec.decode(&mut buf).unwrap();

            assert!(res.is_none());
            assert_eq!(1, buf.len());
        }

        #[test]
        fn decode_empty_server_message() {
            let mut codec = ServerCodec::default();
            let mut buf = BytesMut::new();
            assert_eq!(0, buf.len());

            let res = codec.decode(&mut buf).unwrap();

            assert!(res.is_none());
            assert_eq!(0, buf.len());
        }

        #[test]
        fn decode_single_byte_server_message() {
            let mut codec = ServerCodec::default();
            let mut buf = BytesMut::from(vec![0x00]);
            assert_eq!(1, buf.len());

            let res = codec.decode(&mut buf).unwrap();

            assert!(res.is_none());
            assert_eq!(1, buf.len());
        }

        #[test]
        fn decode_partly_received_server_message_0x16() {
            let mut codec = ServerCodec::default();
            let mut buf = BytesMut::from(vec![
                0x12, // slave address
                0x16, // function code
            ]);
            assert_eq!(buf.len(), 2);

            let res = codec.decode(&mut buf).unwrap();

            assert!(res.is_none());
            assert_eq!(buf.len(), 2);
        }

        #[test]
        fn decode_partly_received_server_message_0x0f() {
            let mut codec = ServerCodec::default();
            let mut buf = BytesMut::from(vec![
                0x12, // slave address
                0x0F, // function code
            ]);
            assert_eq!(buf.len(), 2);

            let res = codec.decode(&mut buf).unwrap();

            assert!(res.is_none());
            assert_eq!(buf.len(), 2);
        }

        #[test]
        fn decode_partly_received_server_message_0x10() {
            let mut codec = ServerCodec::default();
            let mut buf = BytesMut::from(vec![
                0x12, // slave address
                0x10, // function code
            ]);
            assert_eq!(buf.len(), 2);

            let res = codec.decode(&mut buf).unwrap();

            assert!(res.is_none());
            assert_eq!(buf.len(), 2);
        }

        #[test]
        fn decode_rtu_message() {
            let mut codec = ClientCodec::default();
            let mut buf = BytesMut::from(vec![
                0x01, // slave address
                0x03, // function code
                0x04, // byte count
                0x89, //
                0x02, //
                0x42, //
                0xC7, //
                0x00, // crc
                0x9D, // crc
                0x00,
            ]);
            let ResponseAdu { hdr, pdu } = codec.decode(&mut buf).unwrap().unwrap();
            assert_eq!(buf.len(), 1);
            assert_eq!(hdr.slave_id, 0x01);
            if let Ok(Response::ReadHoldingRegisters(data)) = pdu.into() {
                assert_eq!(data.len(), 2);
                assert_eq!(data, vec![0x8902, 0x42C7]);
            } else {
                panic!("unexpected response")
            }
        }

        #[test]
        fn decode_rtu_response_drop_invalid_bytes() {
            let _ = env_logger::init();
            let mut codec = ClientCodec::default();
            let mut buf = BytesMut::from(vec![
                0x42, // dropped byte
                0x43, // dropped byte
                0x01, // slave address
                0x03, // function code
                0x04, // byte count
                0x89, //
                0x02, //
                0x42, //
                0xC7, //
                0x00, // crc
                0x9D, // crc
                0x00,
            ]);
            let ResponseAdu { hdr, pdu } = codec.decode(&mut buf).unwrap().unwrap();
            assert_eq!(buf.len(), 1);
            assert_eq!(hdr.slave_id, 0x01);
            if let Ok(Response::ReadHoldingRegisters(data)) = pdu.into() {
                assert_eq!(data.len(), 2);
                assert_eq!(data, vec![0x8902, 0x42C7]);
            } else {
                panic!("unexpected response")
            }
        }

        #[test]
        fn decode_exception_message() {
            let mut codec = ClientCodec::default();
            let mut buf = BytesMut::from(vec![
                0x66, //
                0x82, // exception = 0x80 + 0x02
                0x03, //
                0xB1, // crc
                0x7E, // crc
            ]);

            let ResponseAdu { pdu, .. } = codec.decode(&mut buf).unwrap().unwrap();
            if let ResponsePdu(Err(err)) = pdu {
                assert_eq!(format!("{}", err), "Modbus function 2: Illegal data value");
                assert_eq!(buf.len(), 0);
            } else {
                panic!("unexpected response")
            }
        }

        #[test]
        fn encode_read_request() {
            let mut codec = ClientCodec::default();
            let mut buf = BytesMut::new();
            let req = Request::ReadHoldingRegisters(0x082b, 2);
            let pdu = req.clone().into();
            let slave_id = 0x01;
            let hdr = Header { slave_id };
            let adu = RequestAdu { hdr, pdu };
            codec.encode(adu.clone(), &mut buf).unwrap();

            assert_eq!(
                buf,
                Bytes::from_static(&[0x01, 0x03, 0x08, 0x2B, 0x00, 0x02, 0xB6, 0x63])
            );
        }

        #[test]
        fn encode_with_limited_buf_capacity() {
            let mut codec = ClientCodec::default();
            let req = Request::ReadHoldingRegisters(0x082b, 2);
            let pdu = req.clone().into();
            let slave_id = 0x01;
            let hdr = Header { slave_id };
            let adu = RequestAdu { hdr, pdu };
            let mut buf = BytesMut::with_capacity(40);
            unsafe {
                buf.set_len(33);
            }
            assert!(codec.encode(adu, &mut buf).is_ok());
        }
    }
}
