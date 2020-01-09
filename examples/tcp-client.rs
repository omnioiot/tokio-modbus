#[cfg(feature = "tcp")]
#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use tokio_modbus::prelude::*;

    let socket_addr = "127.0.0.1:5502".parse().unwrap();

    let mut ctx = tcp::connect(socket_addr).await?;

    println!("Fetching the coupler ID");
    let data = ctx.read_input_registers(0x1000, 7).await?;

    let bytes: Vec<u8> = data.iter().fold(vec![], |mut x, elem| {
        x.push((elem & 0xff) as u8);
        x.push((elem >> 8) as u8);
        x
    });
    let id = String::from_utf8(bytes).unwrap();
    println!("The coupler ID is '{}'", id);

    Ok(())
}

#[cfg(not(feature = "tcp"))]
pub fn main() {
    println!("feature `tcp` is required to run this example");
    std::process::exit(1);
}
