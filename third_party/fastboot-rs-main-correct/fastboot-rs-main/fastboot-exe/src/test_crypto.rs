
mod crypto;

fn main() {
    println!("测试自动密钥生成...");
    let key = crypto::load_or_generate_private_key();
    println!("密钥生成成功: {:?}", key);

    let pub_key = crypto::get_public_key();
    println!("公钥获取成功，长度: {}", pub_key.len());

    println!("测试完成");
}
