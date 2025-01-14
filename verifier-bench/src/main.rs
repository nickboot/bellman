// --prove                  Benchmark prover
// --verify                 Benchmark verifier
// --proofs <num>           Sets number of proofs in a batch
// --public <num>           Sets number of public inputs
// --private <num>          Sets number of private inputs
// --gpu                    Enables GPU
// --samples                Number of runs
// --dummy                  Skip param generation and generate dummy params/proofs
use std::sync::Arc;
use std::time::Instant;

use bellperson::groth16::{
    aggregate::{aggregate_proofs, setup_fake_srs, verify_aggregate_proof},
    create_random_proof_batch, generate_random_parameters, prepare_verifying_key,
    verify_proofs_batch, Parameters, Proof, VerifyingKey,
};
use bellperson::{Circuit, ConstraintSystem, SynthesisError};
use blstrs::{Bls12, Scalar as Fr};
use ff::Field;
use group::{Curve, Group};
use pairing::{Engine, MultiMillerLoop};
use rand::RngCore;
use structopt::StructOpt;

macro_rules! timer {
    ($e:expr) => {{
        let before = Instant::now();
        let ret = $e;
        (
            ret,
            (before.elapsed().as_secs() * 1000 as u64 + before.elapsed().subsec_millis() as u64),
        )
    }};
}

#[derive(Clone)]
pub struct DummyDemo {
    pub public: usize,
    pub private: usize,
}

impl<E: Engine> Circuit<E> for DummyDemo {
    fn synthesize<CS: ConstraintSystem<E>>(self, cs: &mut CS) -> Result<(), SynthesisError> {
        assert!(self.public >= 1);
        let mut x_val = E::Fr::from(2);
        let mut x = cs.alloc_input(|| "", || Ok(x_val))?;
        let mut pubs = 1;

        for _ in 0..self.private + self.public - 1 {
            // Allocate: x * x = x2
            let x2_val = x_val.square();

            let x2 = if pubs < self.public {
                pubs += 1;
                cs.alloc_input(|| "", || Ok(x2_val))?
            } else {
                cs.alloc(|| "", || Ok(x2_val))?
            };

            // Enforce: x * x = x2
            cs.enforce(|| "", |lc| lc + x, |lc| lc + x, |lc| lc + x2);

            x = x2;
            x_val = x2_val;
        }

        cs.enforce(
            || "",
            |lc| lc + (x_val, CS::one()),
            |lc| lc + CS::one(),
            |lc| lc + x,
        );

        Ok(())
    }
}

fn random_points<C, R>(count: usize, mut rng: R) -> Vec<C::AffineRepr>
where
    C: Curve,
    <C as Curve>::AffineRepr: Clone,
    R: RngCore,
{
    // Number of distinct points is limited because generating random points is very time
    // consuming, so it's better to just repeat them.
    const DISTINT_POINTS: usize = 100;
    (0..DISTINT_POINTS)
        .map(|_| C::random(&mut rng).to_affine())
        .collect::<Vec<_>>()
        .into_iter()
        .cycle()
        .take(count)
        .collect()
}

fn dummy_proofs<E: Engine, R: RngCore>(count: usize, mut rng: R) -> Vec<Proof<E>> {
    (0..count)
        .map(|_| Proof {
            a: E::G1::random(&mut rng).to_affine(),
            b: E::G2::random(&mut rng).to_affine(),
            c: E::G1::random(&mut rng).to_affine(),
        })
        .collect()
}

fn dummy_inputs<E: Engine, R: RngCore>(count: usize, mut rng: R) -> Vec<<E as Engine>::Fr> {
    (0..count)
        .map(|_| <E as Engine>::Fr::random(&mut rng))
        .collect()
}

fn dummy_vk<E: MultiMillerLoop, R: RngCore>(public: usize, mut rng: R) -> VerifyingKey<E> {
    VerifyingKey {
        alpha_g1: E::G1::random(&mut rng).to_affine(),
        beta_g1: E::G1::random(&mut rng).to_affine(),
        beta_g2: E::G2::random(&mut rng).to_affine(),
        gamma_g2: E::G2::random(&mut rng).to_affine(),
        delta_g1: E::G1::random(&mut rng).to_affine(),
        delta_g2: E::G2::random(&mut rng).to_affine(),
        ic: random_points::<E::G1, _>(public + 1, &mut rng),
    }
}

fn dummy_params<E, R>(public: usize, private: usize, mut rng: R) -> Parameters<E>
where
    E: MultiMillerLoop,
    R: RngCore,
{
    let count = public + private;
    let hlen = (1 << (((count + public + 1) as f64).log2().ceil() as usize)) - 1;
    Parameters {
        vk: dummy_vk(public, &mut rng),
        h: Arc::new(random_points::<E::G1, _>(hlen, &mut rng)),
        l: Arc::new(random_points::<E::G1, _>(private, &mut rng)),
        a: Arc::new(random_points::<E::G1, _>(count, &mut rng)),
        b_g1: Arc::new(random_points::<E::G1, _>(count, &mut rng)),
        b_g2: Arc::new(random_points::<E::G2, _>(count, &mut rng)),
    }
}

#[derive(Debug, StructOpt, Clone, Copy)]
#[structopt(name = "Bellman Bench", about = "Benchmarking Bellman.")]
struct Opts {
    #[structopt(long = "proofs", default_value = "1")]
    proofs: usize,
    #[structopt(long = "public", default_value = "1")]
    public: usize,
    #[structopt(long = "private", default_value = "1000000")]
    private: usize,
    #[structopt(long = "samples", default_value = "10")]
    samples: usize,
    #[structopt(long = "gpu")]
    gpu: bool,
    #[structopt(long = "verify")]
    verify: bool,
    #[structopt(long = "prove")]
    prove: bool,
    #[structopt(long = "dummy")]
    dummy: bool,
    #[structopt(long = "aggregate")]
    aggregate: bool,
}

fn main() {
    let mut rng = rand::rngs::OsRng;
    pretty_env_logger::init_timed();

    let opts = Opts::from_args();
    if opts.gpu {
        std::env::set_var("BELLMAN_VERIFIER", "gpu");
    } else {
        std::env::set_var("BELLMAN_NO_GPU", "1");
    }

    let circuit = DummyDemo {
        public: opts.public,
        private: opts.private,
    };
    let circuits = vec![circuit.clone(); opts.proofs];

    let params = if opts.dummy {
        dummy_params::<Bls12, _>(opts.public, opts.private, &mut rng)
    } else {
        println!("Generating params... (You can skip this by passing `--dummy` flag)");
        generate_random_parameters(circuit, &mut rng).unwrap()
    };
    let pvk = prepare_verifying_key(&params.vk);

    let srs = if opts.aggregate {
        let x = setup_fake_srs(&mut rng, opts.proofs).specialize(opts.proofs);
        Some(x)
    } else {
        None
    };

    if opts.prove {
        println!("Proving...");

        for _ in 0..opts.samples {
            let (_proofs, took) =
                timer!(create_random_proof_batch(circuits.clone(), &params, &mut rng).unwrap());
            println!("Proof generation finished in {}ms", took);
        }
    }

    if opts.verify {
        println!("Verifying...");

        let includes = [1u8; 32];

        let (inputs, proofs, agg_proof) = if opts.dummy {
            let proofs = dummy_proofs::<Bls12, _>(opts.proofs, &mut rng);
            let inputs = dummy_inputs::<Bls12, _>(opts.public, &mut rng);
            let pis = vec![inputs; opts.proofs];

            let agg_proof = srs.as_ref().map(|srs| {
                let (agg, took) =
                    timer!(aggregate_proofs::<Bls12>(&srs.0, &includes, &proofs).unwrap());
                println!("Proof aggregation finished in {}ms", took);
                agg
            });

            (pis, proofs, agg_proof)
        } else {
            let mut inputs = Vec::new();
            let mut num = Fr::one();
            num = num.double();
            for _ in 0..opts.public {
                inputs.push(num);
                num = num.square();
            }
            println!("(Generating valid proofs...)");
            let (proofs, took) =
                timer!(create_random_proof_batch(circuits, &params, &mut rng).unwrap());
            println!("Proof generation finished in {}ms", took);

            let pis = vec![inputs; opts.proofs];

            let agg_proof = srs.as_ref().map(|srs| {
                let (agg, took) =
                    timer!(aggregate_proofs::<Bls12>(&srs.0, &includes, &proofs).unwrap());
                println!("Proof aggregation finished in {}ms", took);
                agg
            });

            (pis, proofs, agg_proof)
        };

        for _ in 0..opts.samples {
            let pref = proofs.iter().collect::<Vec<&_>>();
            println!(
                "{} proofs, each having {} public inputs...",
                opts.proofs, opts.public
            );

            let (valid, took) =
                timer!(verify_proofs_batch(&pvk, &mut rng, &pref[..], &inputs).unwrap());
            println!(
                "Verification finished in {}ms (Valid: {}) (Proof Size: {} bytes)",
                took,
                valid,
                proofs.len() * Proof::<Bls12>::size(),
            );

            if let Some(ref agg_proof) = agg_proof {
                let srs = srs.as_ref().unwrap();
                let (valid, took) = timer!(verify_aggregate_proof(
                    &srs.1, &pvk, rng, &inputs, agg_proof, &includes,
                )
                .unwrap());
                println!(
                    "Verification aggregated finished in {}ms (Valid: {}) (Proof Size: {} bytes, {})",
                    took,
                    valid,
                    bincode::serialize(agg_proof).unwrap().len(),
                    agg_proof.serialized_len(),
                );
            }
        }
    }
}
