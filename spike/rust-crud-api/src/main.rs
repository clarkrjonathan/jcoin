use axum::{
    routing::get,
    Json,
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Sha256, Digest};

use rust_crud_api::Transaction;
use primitive_types::{U256, U512};

use generic_array::{GenericArray, typenum::U32};

#[tokio::main]
async fn main() {
    // Router::new() is an associated function (similar to a static method).
    // It creates and returns an empty Router.
    //
    // get(get_handler) returns a MethodRouter.
    // .post(transaction_handler) is a method on that MethodRouter.
    let app = Router::new()
        .route(
            "/transaction",
            get(get_handler).post(transaction_handler),
        );

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .unwrap();

    println!("Server started successfully at 0.0.0.0:3000");

    axum::serve(listener, app).await.unwrap();
}

// Json(body): Json<Transaction>
//
// Json<Transaction> is the type Axum gives the handler.
// Json(body) destructures the Json wrapper and puts the
// Transaction inside it into the variable "body".
//
// Therefore, body has type Transaction.
async fn transaction_handler(Json(body): Json<Transaction>) {
    //& is a non mutable reference. This allows the method to look
    //at the value without owning it, but it cannot mutate it because
    //we didn't do &mut. &mut is equivelant to how objects are passed
    //in java by default
    let json_body = serde_json::to_string_pretty(&body).unwrap();

    if(body.nonce == 1212) {
        let nonce = quick_luh_mine(U256::from(50_u32), json_body);
        
        if body.verify_signature() {
            println!("Signature is valid!");
        }

        println!("Found nonce: {}", nonce);

        
        return;
    }

    println!("Received transaction:");
    println!("{}", json_body);
}

async fn get_handler() -> Json<serde_json::Value> {
    let json_response = json!({
        "status": "ok",
        "message": "Hello, World!"
    });

    Json(json_response)
}

fn quick_luh_mine(difficulty: U256, input: String) -> u32 {
    assert!(!difficulty.is_zero());
    let target = U256::MAX / difficulty;

    let mut nonce: u32 = 0;
    loop {
        let mut hasher = Sha256::new();
        hasher.update(input.as_bytes());
        hasher.update(nonce.to_string().as_bytes());
        let hash = hasher.finalize();

        // SHA-256 output is big-endian, so this is a straight numeric compare
        if U256::from_big_endian(&hash) <= target {
            return nonce;
        }
        nonce += 1;
    }
}