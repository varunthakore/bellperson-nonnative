mod entropy;

pub mod helper {

    use num_bigint::BigUint;
    use num_integer::Integer;
    use num_traits::One;
    use sapling_crypto::bellman::pairing::ff::{Field, PrimeField};

    use super::entropy::helper::EntropySource;
    use super::entropy::NatTemplate;
    use hash::hashes::mimc;
    use hash::low_k_bits;
    use hash::miller_rabin_prime::helper::miller_rabin_32b;
    use hash::Hasher;
    use util::convert::f_to_nat;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PocklingtonPlan {
        pub nonce_bits: usize,
        pub initial_entropy: usize,
        pub extensions: Vec<PlannedExtension>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PlannedExtension {
        pub nonce_bits: usize,
        pub random_bits: usize,
    }

    /// Returns the probability that a number with `bits` bits is prime
    fn prime_density(bits: usize) -> f64 {
        use std::f64::consts::E;
        let log2e = E.log2();
        let b = bits as f64;
        log2e / b - log2e * log2e / b / b
    }

    /// Returns the number of random `bits`-bit numbers that must be checked to find a prime with
    /// all but `p_fail` probability
    pub fn prime_trials(bits: usize, p_fail: f64) -> usize {
        let p = prime_density(bits);
        (p_fail.log(1.0 - p).ceil() + 0.1) as usize
    }

    /// The number of nonce bits needed to generate a `bits`-bit prime with all but 2**-64
    /// probability.
    pub fn nonce_bits_needed(bits: usize) -> usize {
        let trials = prime_trials(bits, 2.0f64.powi(-64));
        ((trials as f64).log2().ceil() + 0.1) as usize
    }

    impl PocklingtonPlan {
        /// Given a target entropy, constructs a plan for how to make a prime number of that
        /// bitwidth that can be certified using a recursive Pocklington test.
        pub fn new(entropy: usize) -> Self {
            // (entropy, bits, extension)
            assert!(entropy >= 29);
            #[derive(Debug)]
            struct Entry {
                marginal_entropy: usize,
                entropy: usize,
                bits: usize,
                random_bits: usize,
                nonce_bits: usize,
            }
            let bits = 32;
            let nonce_bits = nonce_bits_needed(bits) - 1;
            let mut table: Vec<Entry> = vec![Entry {
                entropy: bits - 3 - nonce_bits,
                marginal_entropy: bits - 3 - nonce_bits,
                bits,
                random_bits: 0,
                nonce_bits: nonce_bits,
            }];
            while table.last().unwrap().entropy < entropy {
                let mut next = Entry {
                    entropy: table.last().unwrap().entropy + 1,
                    marginal_entropy: 0,
                    bits: std::usize::MAX,
                    random_bits: 0,
                    nonce_bits: 0,
                };
                for base in table.as_slice().iter().rev() {
                    let random_bits = next.entropy - base.entropy;
                    let mut error = false;
                    let mut nonce_bits = 0;
                    let mut next_bits = 0;
                    loop {
                        if random_bits + nonce_bits + 1 >= base.bits {
                            error = true;
                            break;
                        }
                        next_bits = nonce_bits + random_bits + base.bits + 1;
                        if nonce_bits >= nonce_bits_needed(next_bits) {
                            break;
                        }
                        nonce_bits += 1;
                    }
                    if !error && next_bits < next.bits {
                        next.bits = next_bits;
                        next.marginal_entropy = random_bits;
                        next.random_bits = random_bits + nonce_bits;
                        next.nonce_bits = nonce_bits;
                    }
                }
                assert_ne!(next.bits, std::usize::MAX);
                table.push(next);
            }
            assert_eq!(table.last().unwrap().entropy, entropy);
            let mut i = table.len() - 1;
            let mut extensions = Vec::new();
            while i > 0 {
                extensions.push(PlannedExtension {
                    nonce_bits: table[i].nonce_bits,
                    random_bits: table[i].random_bits,
                });
                i -= table[i].marginal_entropy;
            }
            extensions.reverse();
            Self {
                initial_entropy: 29 - nonce_bits,
                nonce_bits,
                extensions,
            }
        }

        pub fn entropy(&self) -> usize {
            self.extensions
                .iter()
                .map(|i| i.random_bits - i.nonce_bits)
                .sum::<usize>()
                + self.initial_entropy
        }

        pub fn max_bits(&self) -> usize {
            self.extensions
                .iter()
                .map(|i| i.random_bits + 1)
                .sum::<usize>()
                + 32
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PocklingtonExtension {
        pub plan: PlannedExtension,
        pub random: BigUint,
        pub nonce: usize,
        pub checking_base: BigUint,
        pub result: BigUint,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PocklingtonCertificate {
        pub base_prime: BigUint,
        pub base_nonce: usize,
        pub extensions: Vec<PocklingtonExtension>,
    }

    impl PocklingtonCertificate {
        pub fn number(&self) -> &BigUint {
            if let Some(l) = self.extensions.last() {
                &l.result
            } else {
                &self.base_prime
            }
        }
    }

    pub fn attempt_pocklington_extension<F: PrimeField>(
        mut p: PocklingtonCertificate,
        plan: &PlannedExtension,
        random: BigUint,
    ) -> Result<PocklingtonCertificate, PocklingtonCertificate> {
        for i in 0..(1 << plan.nonce_bits) {
            let nonce = i;
            let mimcd_nonce = low_k_bits(
                &f_to_nat(&mimc::helper::permutation(
                    F::from_str(&format!("{}", i)).unwrap(),
                )),
                plan.nonce_bits,
            );
            let nonced_extension = &random + &mimcd_nonce;
            let number = p.number() * &nonced_extension + 1usize;
            let mut base = BigUint::from(2usize);
            while base < number {
                let part = base.modpow(&nonced_extension, &number);
                if part.modpow(p.number(), &number) != BigUint::from(1usize) {
                    break;
                }
                if (&part - 1usize).gcd(&number).is_one() {
                    p.extensions.push(PocklingtonExtension {
                        plan: plan.clone(),
                        random,
                        checking_base: base,
                        result: number,
                        nonce,
                    });
                    return Ok(p);
                }
                base += 1usize;
            }
        }
        Err(p)
    }

    pub fn execute_pocklington_plan<F: PrimeField>(
        hash: F,
        plan: &PocklingtonPlan,
        nonce: usize,
    ) -> Option<PocklingtonCertificate> {
        let mut bits = EntropySource::new(hash, plan.entropy());
        let base_nat = bits.get_bits_as_nat(NatTemplate {
            trailing_ones: 2,
            leading_ones: 1,
            random_bits: 29,
        });
        if !miller_rabin_32b(&base_nat) {
            return None;
        }
        let mut certificate = PocklingtonCertificate {
            base_prime: base_nat,
            base_nonce: nonce,
            extensions: Vec::new(),
        };
        for extension in &plan.extensions {
            let random = bits.get_bits_as_nat(NatTemplate {
                random_bits: extension.random_bits,
                trailing_ones: 0,
                leading_ones: 1,
            });
            certificate =
                attempt_pocklington_extension::<F>(certificate, extension, random).ok()?;
        }
        Some(certificate)
    }

    pub fn hash_to_pocklington_prime<H: Hasher>(
        inputs: &[H::F],
        entropy: usize,
        base_hash: &H,
    ) -> Option<PocklingtonCertificate> {
        let plan = PocklingtonPlan::new(entropy);
        let mut inputs: Vec<H::F> = inputs.iter().copied().collect();
        inputs.push(H::F::zero());
        for nonce in 0..(1 << plan.nonce_bits) {
            let hash = base_hash.hash(&inputs);
            if let Some(cert) = execute_pocklington_plan(hash, &plan, nonce) {
                return Some(cert);
            }
            inputs.last_mut().unwrap().add_assign(&H::F::one());
        }
        None
    }

    #[cfg(test)]
    mod test {
        use super::*;

        #[test]
        fn prime_prob_64b() {
            let p = prime_density(64);
            assert!(p >= 0.02);
            assert!(p <= 0.03);
        }

        #[test]
        fn prime_trials_64b() {
            let t = prime_trials(64, (2.0f64).powi(64));
            assert!(t >= 1000);
            assert!(t <= 1100);
        }
    }
}

use num_bigint::BigUint;
use num_traits::One;
use sapling_crypto::bellman::pairing::ff::Field;
use sapling_crypto::bellman::pairing::Engine;
use sapling_crypto::bellman::{ConstraintSystem, SynthesisError};
use sapling_crypto::circuit::boolean::Boolean;
use sapling_crypto::circuit::num::AllocatedNum;

use self::entropy::{EntropySource, NatTemplate};
use hash::circuit::CircuitHasher;
use hash::hashes::mimc;
use hash::Hasher;
use mp::bignat::{BigNat, BigNatParams};
use util::convert::usize_to_f;
use util::gadget::Gadget;
use util::num::Num;
use OptionExt;

pub fn hash_to_pocklington_prime<
    E: Engine,
    H: Hasher<F = E::Fr> + CircuitHasher<E = E>,
    CS: ConstraintSystem<E>,
>(
    mut cs: CS,
    input: &[AllocatedNum<E>],
    limb_width: usize,
    entropy: usize,
    base_hash: &H,
) -> Result<BigNat<E>, SynthesisError> {
    use self::helper::{PocklingtonCertificate, PocklingtonPlan};
    let plan = PocklingtonPlan::new(entropy);
    let cert: Option<PocklingtonCertificate> = input
        .iter()
        .map(|n| n.get_value().clone())
        .collect::<Option<Vec<E::Fr>>>()
        .and_then(|is| helper::hash_to_pocklington_prime(&is, entropy, base_hash));
    let base_nonce = AllocatedNum::alloc(cs.namespace(|| "nonce"), || {
        Ok(usize_to_f::<E::Fr>(cert.as_ref().grab()?.base_nonce))
    })?;
    let mut inputs = input.to_vec();
    inputs.push(base_nonce);
    let hash = base_hash.allocate_hash(cs.namespace(|| "base hash"), &inputs)?;
    let mut entropy_source =
        EntropySource::alloc(cs.namespace(|| "entropy source"), Some(&()), hash, &entropy)?;

    let mut prime = entropy_source.get_bits_as_nat::<CS>(
        NatTemplate {
            trailing_ones: 2,
            leading_ones: 1,
            random_bits: 29,
        },
        limb_width,
    );
    let mr_res = &prime.miller_rabin_32b(cs.namespace(|| "base check"))?;
    Boolean::enforce_equal(
        cs.namespace(|| "MR passes"),
        &mr_res,
        &Boolean::constant(true),
    )?;
    for (i, extension) in plan.extensions.into_iter().enumerate() {
        let mut cs = cs.namespace(|| format!("extension {}", i));
        let nonce = AllocatedNum::alloc(cs.namespace(|| "nonce"), || {
            Ok(usize_to_f(cert.as_ref().grab()?.extensions[i].nonce))
        })?;
        let mimcd_nonce_all_bits = Num::from(mimc::permutation(cs.namespace(|| "mimc"), nonce)?);
        let mimcd_nonce = BigNat::from_num(
            mimcd_nonce_all_bits
                .low_k_bits(cs.namespace(|| "mimc low bits"), extension.nonce_bits)?,
            BigNatParams {
                n_limbs: 1,
                limb_width: prime.params.limb_width,
                max_word: BigUint::one() << extension.nonce_bits,
                min_bits: 0,
            },
        );
        let extension = entropy_source.get_bits_as_nat::<CS>(
            NatTemplate {
                random_bits: extension.random_bits,
                trailing_ones: 0,
                leading_ones: 1,
            },
            limb_width,
        );
        let nonced_extension = extension.add::<CS>(&mimcd_nonce)?;
        let base = BigNat::alloc_from_nat(
            cs.namespace(|| "base"),
            || {
                Ok(BigUint::from(
                    cert.as_ref().grab()?.extensions[i].checking_base.clone(),
                ))
            },
            limb_width,
            1, // TODO allow larger bases
        )?;
        let n_less_one = nonced_extension.mult(cs.namespace(|| "n - 1"), &prime)?;
        let n = n_less_one.shift::<CS>(E::Fr::one());
        let part = base.pow_mod(cs.namespace(|| "a^r"), &nonced_extension, &n)?;
        let one = BigNat::one(cs.namespace(|| "one"), prime.params().limb_width)?;
        let part_less_one = part.sub(cs.namespace(|| "a^r - 1"), &one)?;
        part_less_one.enforce_coprime(cs.namespace(|| "coprime"), &n)?;
        let power = part.pow_mod(cs.namespace(|| "a^r^p"), &prime, &n)?;
        power.equal_when_carried(cs.namespace(|| "a^r^p == 1"), &one)?;
        prime = n;
    }
    Ok(prime)
}

#[cfg(test)]
mod test {
    use super::{hash_to_pocklington_prime, helper};
    use sapling_crypto::bellman::pairing::ff::{PrimeField, ScalarEngine};
    use sapling_crypto::bellman::pairing::Engine;
    use sapling_crypto::bellman::{ConstraintSystem, SynthesisError};
    use sapling_crypto::circuit::num::AllocatedNum;

    use hash::circuit::CircuitHasher;
    use hash::hashes::Poseidon;
    use hash::{miller_rabin_prime, Hasher};
    use mp::bignat::BigNat;
    use OptionExt;

    use util::test_helpers::*;

    #[test]
    fn pocklington_plan_128() {
        let p = helper::PocklingtonPlan::new(128);
        println!("{:#?}", p);
        println!("{:#?}", p.max_bits());
        assert_eq!(p.entropy(), 128);
    }

    #[test]
    fn pocklington_plan_256() {
        let p = helper::PocklingtonPlan::new(256);
        println!("{:#?}", p);
        println!("{:#?}", p.max_bits());
        assert_eq!(p.entropy(), 256);
    }

    //#[test]
    //fn pocklington_extension_0() {
    //    let cert = Base(BigUint::from(241usize));
    //    let extension = BigUint::from(6usize);
    //    let ex = Ok(Recursive {
    //        rec: Box::new(cert.clone()),
    //        number: BigUint::from(1447usize),
    //        base: BigUint::from(2usize),
    //        extension: extension.clone(),
    //        nonce: 0,
    //    });
    //    let act = helper::attempt_pocklington_extension::<<Bn256 as ScalarEngine>::Fr>(cert, extension);
    //    assert_eq!(ex, act);
    //}

    macro_rules! pocklington_hash_tests {
        ($($name:ident: $value:expr,)*) => {
            $(
                #[test]
                fn $name() {
                    let (inputs, entropy) = $value;
                    let input_values: Vec<<Bn256 as ScalarEngine>::Fr> = inputs
                        .iter()
                        .map(|s| <Bn256 as ScalarEngine>::Fr::from_str(s).unwrap())
                        .collect();
                    let hash = Poseidon::<Bn256>::default();
                    let cert = helper::hash_to_pocklington_prime(&input_values, entropy, &hash).expect("pocklington generation failed");
                    assert!(miller_rabin_prime::helper::miller_rabin(cert.number(), 20));
                }
            )*
        }
    }

    pocklington_hash_tests! {
        pocklington_hash_helper_128_1: (&["1"], 128),
        pocklington_hash_helper_128_2: (&["2"], 128),
        pocklington_hash_helper_128_3: (&["3"], 128),
        pocklington_hash_helper_128_4: (&["4"], 128),
    }

    #[derive(Debug)]
    pub struct PockHashInputs<'a> {
        pub inputs: &'a [&'a str],
    }

    #[derive(Debug)]
    pub struct PockHashParams<H> {
        pub entropy: usize,
        pub hash: H,
    }

    pub struct PockHash<'a, H> {
        inputs: Option<PockHashInputs<'a>>,
        params: PockHashParams<H>,
    }

    impl<'a, E: Engine, H: Hasher<F = E::Fr> + CircuitHasher<E = E>> Circuit<E> for PockHash<'a, H> {
        fn synthesize<CS: ConstraintSystem<E>>(self, cs: &mut CS) -> Result<(), SynthesisError> {
            let input_values: Vec<E::Fr> = self
                .inputs
                .grab()?
                .inputs
                .iter()
                .map(|s| E::Fr::from_str(s).unwrap())
                .collect();
            let cert = helper::hash_to_pocklington_prime(
                &input_values,
                self.params.entropy,
                &self.params.hash,
            )
            .expect("pocklington hash failed");
            let plan = helper::PocklingtonPlan::new(self.params.entropy);
            let allocated_expected_output = BigNat::alloc_from_nat(
                cs.namespace(|| "output"),
                || Ok(cert.number().clone()),
                32,
                (plan.max_bits() - 1) / 32 + 1,
            )?;
            let allocated_inputs: Vec<AllocatedNum<E>> = input_values
                .into_iter()
                .enumerate()
                .map(|(i, value)| {
                    AllocatedNum::alloc(cs.namespace(|| format!("input {}", i)), || Ok(value))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let hash = hash_to_pocklington_prime(
                cs.namespace(|| "hash"),
                &allocated_inputs,
                32,
                self.params.entropy,
                &self.params.hash,
            )?;
            println!(
                "Pocklington bits in: [{}, {}]",
                hash.params.min_bits,
                hash.params.limb_width * hash.params.n_limbs
            );
            hash.equal(cs.namespace(|| "eq"), &allocated_expected_output)?;
            Ok(())
        }
    }

    circuit_tests! {
        pocklington_hash_29_1: (
            PockHash {
                inputs: Some(PockHashInputs {
                    inputs: &["1","2","3","4","5","6","7","8","9","10"],
                }),
                params: PockHashParams {
                    entropy: 29,
                    hash: Poseidon::default(),
                },
            },
            true,
        ),
        pocklington_hash_30_1: (
            PockHash {
                inputs: Some(PockHashInputs {
                    inputs: &["1","2","3","4","5","6","7","8","9","10"],
                }),
                params: PockHashParams {
                    entropy: 30,
                    hash: Poseidon::default(),
                },
            },
            true,
        ),
        pocklington_hash_50_1: (
            PockHash {
                inputs: Some(PockHashInputs {
                    inputs: &["1","2","3","4","5","6","7","8","9","10"],
                }),
                params: PockHashParams {
                    entropy: 50,
                    hash: Poseidon::default(),
                },
            },
            true,
        ),
        pocklington_hash_80_1: (
            PockHash {
                inputs: Some(PockHashInputs {
                    inputs: &["1","2","3","4","5","6","7","8","9","10"],
                }),
                params: PockHashParams {
                    entropy: 80,
                    hash: Poseidon::default(),
                },
            },
            true,
        ),
        pocklington_hash_128_1: (
            PockHash {
                inputs: Some(PockHashInputs {
                    inputs: &["1","2","3","4","5","6","7","8","9","10"],
                }),
                params: PockHashParams {
                    entropy: 128,
                    hash: Poseidon::default(),
                },
            },
            true,
        ),
        pocklington_hash_256_1: (
            PockHash {
                inputs: Some(PockHashInputs {
                    inputs: &["1","2","3","4","5","6","7","8","9","10"],
                }),
                params: PockHashParams {
                    entropy: 256,
                    hash: Poseidon::default(),
                },
            },
            true,
        ),
    }
}