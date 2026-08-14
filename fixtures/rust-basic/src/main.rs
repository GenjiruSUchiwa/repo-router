mod auth;
mod db;

use auth::token::verify_token;

fn main() {
    let token = "sample.jwt.token";
    let is_valid = verify_token(token);
    println!("Token valid: {is_valid}");
}
