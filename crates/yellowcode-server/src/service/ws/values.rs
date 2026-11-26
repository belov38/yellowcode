use super::XdtoType;
use super::soap::Param;
use crate::process::ProcessError;
use crate::service::wire::Unimplemented;
use bsl_vm::{Exception, Execution, TypeId, Value};
use std::sync::Arc;

type Converted<T> = Result<Result<T, Unimplemented>, ProcessError>;

const NOT_CONVERTED: Unimplemented =
    Unimplemented("a SOAP parameter value that does not convert to its XDTO type");

fn conversion<T>(r: Result<T, Exception>) -> Converted<T> {
    match r {
        Ok(v) => Ok(Ok(v)),
        Err(e @ Exception::Fatal(_)) => Err(e.into()),
        Err(_) => Ok(Err(NOT_CONVERTED)),
    }
}

pub fn argument(
    ex: &mut Execution,
    xdto: &XdtoType,
    nillable: bool,
    sent: &Param,
) -> Converted<Value> {
    if sent.nil {
        return Ok(if !nillable {
            Err(Unimplemented("xsi:nil on a parameter without Nillable"))
        } else if *xdto == XdtoType::SubscriberId {
            Ok(Value::Undefined)
        } else {
            Err(Unimplemented(
                "xsi:nil on a parameter of a type other than DeliverableNotificationSubscriberID",
            ))
        });
    }
    let t = match xdto {
        XdtoType::String => TypeId::STRING,
        XdtoType::Decimal => TypeId::NUMBER,
        XdtoType::Boolean => TypeId::BOOL,
        XdtoType::DateTime => TypeId::DATE,
        XdtoType::ValueStorage => {
            let units: Vec<u16> = sent.text.encode_utf16().collect();
            return Ok(match bsl_lib::b64::base64_value(&units) {
                Some(bytes) if !bytes.is_empty() => {
                    Ok(bsl_stdlib::value_storage_from_bytes(Arc::from(bytes)))
                }
                _ => Err(NOT_CONVERTED),
            });
        }
        XdtoType::Other(_) | XdtoType::SubscriberId => {
            return Ok(Err(Unimplemented(
                "a SOAP parameter of an XDTO type the replica does not serve",
            )));
        }
    };
    conversion(bsl_stdlib::from_xml_string(ex, t, &sent.text))
}

pub fn answer(ex: &mut Execution, xdto: &XdtoType, value: &Value) -> Converted<String> {
    if matches!(value, Value::Undefined) {
        return Ok(Err(Unimplemented(
            "an Undefined return value of a web service operation (xsi:nil)",
        )));
    }
    let fits = match xdto {
        XdtoType::String => matches!(value, Value::String(_)),
        XdtoType::Decimal => matches!(value, Value::Number(_)),
        XdtoType::Boolean => matches!(value, Value::Bool(_)),
        XdtoType::DateTime => matches!(value, Value::Date(_)),
        XdtoType::ValueStorage => bsl_stdlib::value_storage_bytes(value).is_some(),
        XdtoType::Other(_) | XdtoType::SubscriberId => false,
    };
    if !fits {
        return Ok(Err(Unimplemented(
            "a return value of a type other than the XDTO return type of the operation",
        )));
    }
    Ok(Ok(bsl_stdlib::to_xml_string(ex, value)?))
}
