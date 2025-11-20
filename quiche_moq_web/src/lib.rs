use log::info;
use octets::{Octets, OctetsMut};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys;
use web_sys::{Document, HtmlVideoElement, ReadableStreamDefaultReader, WebTransport, WebTransportBidirectionalStream, Window};
use web_sys::js_sys::{Object, Reflect, Uint8Array};
use quiche_moq_wire::control_message::{ClientSetupMessage, ControlMessage};
use quiche_moq_wire::{FromBytes, SetupParameters, ToBytes, MOQ_VERSION_DRAFT_13};

#[wasm_bindgen]
pub async fn start() -> Result<(), JsValue> {
    console_log::init_with_level(log::Level::Trace).unwrap();
    info!("starting");
    let window: Window = web_sys::window().unwrap();
    let document: Document = window.document().unwrap();
    let video: HtmlVideoElement = document
        .create_element("video")?
        .dyn_into::<HtmlVideoElement>()?;

    document.body().unwrap().append_child(&video)?;

    let url = "https://127.0.0.1:8080";
    let wt = WebTransport::new(url)?;
    JsFuture::from(wt.ready()).await?;
    info!("ready");
    let s = JsFuture::from(wt.create_bidirectional_stream()).await?;
    info!("create bidirectional stream");
    let s: WebTransportBidirectionalStream = s.into();
    let w = s.writable();
    let w = w.get_writer()?;

    let version = MOQ_VERSION_DRAFT_13;
    // send setup
    {
        let setup = ClientSetupMessage {
            supported_versions: vec![version],
            setup_parameters: SetupParameters {
                path: None,
                max_request_id: None,
                role: None,
                extra_parameters: vec![],
            },
        };

        let mut buf = [0u8; 1024];
        let mut o = OctetsMut::with_slice(&mut buf);
        setup.to_bytes(&mut o, version).unwrap();
        let len = o.off();
        let chunk = Uint8Array::from(&buf[..len]);
        JsFuture::from(w.write_with_chunk(&chunk)).await?;
        info!("sent moq setup: {:?}", setup);
    }
    // receive setup
    {
        let r = s.readable();
        let r = r.get_reader().dyn_into::<ReadableStreamDefaultReader>()?;
        let res = JsFuture::from(r.read()).await?;
        let res = Object::from(res);
        let done = Reflect::get(&res, &"done".into())?.as_bool().unwrap_or(false);
        assert!(!done);
        let value = Reflect::get(&res, &"value".into())?.dyn_into::<Uint8Array>().unwrap_or(Uint8Array::from([].as_slice()));
        let value = value.to_vec();
        let mut o =  Octets::with_slice(&value.as_slice()[..value.len()]);
        loop {
            let cm = match ControlMessage::from_bytes(&mut o, version) {
                Ok(cm) => cm,
                Err(quiche_moq_wire::Error::Octets(octets::BufferTooShortError)) => break,
                Err(e) => unimplemented!("{:?}", e)
            };
            match cm {
                ControlMessage::ServerSetup(setup) => {
                    info!("received moq setup, {:?}", setup);
                }
                cm => unimplemented!("{:?}", cm),
            }
        }
    }
    Ok(())
}
