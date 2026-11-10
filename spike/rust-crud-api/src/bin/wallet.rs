
use serde::{Deserialize, Serialize};
use serde_json::json;

use std::io::{stdout, Write};
use std::collections::HashMap;
use std::fs;

use ed25519_dalek::{Signer, SigningKey};
use primitive_types::{U256, U512};
use rand::rngs::OsRng;

use rust_crud_api::Transaction;

// The private key (32-byte seed) is stored as hex in this file
const KEY_FILE: &str = "wallet_ed25519";

// Load the wallet key, or generate a new one on the first run
fn load_or_generate_key() -> Result<SigningKey, Box<dyn std::error::Error>> {
    if let Ok(hex_seed) = fs::read_to_string(KEY_FILE) {
        let seed: [u8; 32] = hex::decode(hex_seed.trim())?
            .try_into()
            .map_err(|_| "key file must hold 32 bytes")?;
        return Ok(SigningKey::from_bytes(&seed));
    }

    let signing_key = SigningKey::generate(&mut OsRng);
    fs::write(KEY_FILE, hex::encode(signing_key.to_bytes()))?;
    println!("Generated new key, saved to {}", KEY_FILE);
    Ok(signing_key)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /*
    Main returns a Result enum
    Result enum can be either a successful result Ok() or an error Err()
    This return type is saying that our successful return result will be a () which is like null
    The error type is saying we will return a Box value, a Box value is like an allocation on the heap

    Implementation of it would look something like
    enum Result<T, E> {
        Ok(T), //Ok variant with generic S (Successful) type
        Err(E), //Err variant with generic E (Error) type
    }
    


    Box<dyn std::error::Error> is just a smart pointer of type <dyn std::error::Error>
    Why do we store an error on the heap?
    Well theoretically we could put an error on the heap to pull from another thread,
    I don't believe we do that here

    See dyn notes

    So why this pattern?
    The Return type must be a known size but our Error and Success types may be different
    We handle this issue by making it an enum, see enum notes

    But then the problem within the Return type is that we are generalizing the return type
    This is fine we just use a generic as shown above
    But within this generic type, in our scenario we don't necessarily know what concrete type
    our error is, we instead just know that it implements the std::error::Error trait

    So we can use a dyn to specify that we don't care what concrete struct it is we only care
    that it implements the functions from std::error::Error.

    And then the final problem is that we need to know a concrete size to know if its bigger than ()
    which would mean our Result enum in mem would be larger. So instead of keeping the dyn trait object
    in our Result we create a Box pointer and allocate it on the heap. Now we know exactly how large the
    Box pointer is so we can resolve the size of E and then resolve the size of the Return enum
    */
    

    let client = reqwest::Client::new();

    let signing_key = load_or_generate_key()?;

    // The sender field is the public key, so the node can verify with it
    let mut transaction = Transaction {
        sender: U256::from_big_endian(signing_key.verifying_key().as_bytes()),
        recipient: U256::zero(),
        input: 0,
        output: 0,
        nonce: 1212, // the node only checks the signature for this nonce right now
        signature: U512::zero(),
    };

    // Sign everything except the signature, then fill it in
    let signature = signing_key.sign(&transaction.signing_bytes());
    transaction.signature = U512::from_big_endian(&signature.to_bytes());

    if let Ok(pretty_json) = serde_json::to_string_pretty(&transaction) {
        println!("Pretty:\n{}", pretty_json);
    }

    let response = client
    .post("http://localhost:3000/transaction")
    .header("Content-Type", "application/json") // Match curl -H
    .json(&transaction)                               // Match curl -d (serializes struct to JSON)
    .send()
    .await?; //This ? is where we return the Err() type

    // 6. Parse the JSON response into our struct
    if response.status().is_success() {
        println!("Success!");
    } else {
        println!("Server returned an error status: {}", response.status());
    }

    Ok(())

}



