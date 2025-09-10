# Avi Coin

Avi Coin is a personal project aimed at improving my skills in:

- **Rust**: Enhancing my proficiency in Rust programming.
- **Bitcoin Knowledge**: Understanding how Bitcoin works by implementing a similar coin from scratch.

## Goals

The primary objective of this project is to build a Bitcoin-like cryptocurrency **without referencing any Bitcoin code**, using only Rust. I'll rely on online resources, particularly [bitcoin.org](https://bitcoin.org/), to guide the development.

While this implementation won't match Bitcoin in terms of **security or features**, it will serve as a simplified version covering fundamental concepts.

## Features - Planned in the near future
- [x] Simple block mining
- [ ] Be able to send transactions to another person
- [ ] Create coinbase transaction
- [ ] Create transaction pool
- [ ] Assemble the mined from transaction pool 
- [ ] Send blocks to other peers in the network
- [ ] Store blocks
- [ ] Block validation
- [ ] Adjust difficulty according to mining

## Disclaimer

This project is purely for **learning purposes**. It is **not** intended for real-world use.

---

Stay tuned for updates, and feel free to explore the code!

---
Current feature:
## Send transactions:
Wallets should be able to:
- Generate private keys
- Derive the corresponding public keys
- Helps distribute those public keys as necessary
- Monitors for outputs spent to those public keys (UTXO separate module?)
- Creates and signs transactions spending those outputs 
- Broadcasts the signed transactions
