// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use crypto::{BatchMerkleProof, ElementHasher, Hasher};
use math::FieldElement;
use utils::{
    collections::Vec, string::ToString, ByteReader, ByteWriter, Deserializable,
    DeserializationError, Serializable, SliceReader,
};

// FRI PROOF
// ================================================================================================

/// A proof generated by a FRI prover.
///
/// A FRI proof contains information proving that a function *f* is a polynomial of some bounded
/// degree *d*. FRI proofs cannot be instantiated directly - they must be generated by a instance
/// of a [FriProver](crate::FriProver), and can be verified by a instance of a
/// [FriVerifier](crate::FriVerifier) via [VerifierChannel](crate::VerifierChannel) interface.
///
/// A proof consists of zero or more layers and a remainder polynomial. Each layer contains a set of
/// polynomial evaluations at positions queried by the verifier as well as Merkle authentication
/// paths for these evaluations (the Merkle paths are compressed into a batch Merkle proof). The
/// remainder polynomial is given by its list of coefficients i.e. field elements.
///
/// All values in a proof are stored as vectors of bytes. Thus, the values must be parsed before
/// they can be returned to the user. To do this, [parse_layers()](FriProof::parse_layers())
/// and [parse_remainder()](FriProof::parse_remainder()) methods can be used.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FriProof {
    layers: Vec<FriProofLayer>,
    remainder: Vec<u8>,
    num_partitions: u8, // stored as power of 2
}

impl FriProof {
    // CONSTRUCTOR
    // --------------------------------------------------------------------------------------------
    /// Creates a new FRI proof from the provided layers and remainder polynomial.
    ///
    /// # Panics
    /// Panics if:
    /// * Number of remainder elements zero or is not a power of two.
    /// * `num_partitions` is zero or is not a power of two.
    pub(crate) fn new<E: FieldElement>(
        layers: Vec<FriProofLayer>,
        remainder: Vec<E>,
        num_partitions: usize,
    ) -> Self {
        assert!(!remainder.is_empty(), "number of remainder elements must be greater than zero");
        assert!(
            remainder.len().is_power_of_two(),
            "size of the remainder must be a power of two, but was {}",
            remainder.len()
        );
        assert!(num_partitions > 0, "number of partitions must be greater than zero");
        assert!(
            num_partitions.is_power_of_two(),
            "number of partitions must be a power of two, but was {num_partitions}"
        );
        FriProof {
            layers,
            remainder: remainder.to_bytes(),
            num_partitions: num_partitions.trailing_zeros() as u8,
        }
    }

    /// Creates a dummy `FriProof` for use in tests.
    pub fn new_dummy() -> Self {
        Self {
            layers: Vec::new(),
            remainder: Vec::new(),
            num_partitions: 0,
        }
    }

    // PUBLIC ACCESSORS
    // --------------------------------------------------------------------------------------------

    /// Returns the number of layers in this proof.
    pub fn num_layers(&self) -> usize {
        self.layers.len()
    }

    /// Returns the number of remainder elements in this proof.
    ///
    /// The number of elements is computed by dividing the number of remainder bytes by the size
    /// of the field element specified by `E` type parameter.
    pub fn num_remainder_elements<E: FieldElement>(&self) -> usize {
        self.remainder.len() / E::ELEMENT_BYTES
    }

    /// Returns the number of partitions used during proof generation.
    pub fn num_partitions(&self) -> usize {
        2usize.pow(self.num_partitions as u32)
    }

    /// Returns the size of this proof in bytes.
    pub fn size(&self) -> usize {
        // +1 for number of layers, +1 for remainder length, +1 for number of partitions
        self.layers
            .iter()
            .fold(self.remainder.len() + 3, |acc, layer| acc + layer.size())
    }

    // PARSING
    // --------------------------------------------------------------------------------------------

    /// Decomposes this proof into vectors of query values for each layer and corresponding Merkle
    /// authentication paths for each query (grouped into batch Merkle proofs).
    ///
    /// # Panics
    /// Panics if:
    /// * `domain_size` is not a power of two.
    /// * `folding_factor` is smaller than two or is not a power of two.
    ///
    /// # Errors
    /// Returns an error if:
    /// * This proof is not consistent with the specified `domain_size` and `folding_factor`.
    /// * Any of the layers could not be parsed successfully.
    #[allow(clippy::type_complexity)]
    pub fn parse_layers<H, E>(
        self,
        mut domain_size: usize,
        folding_factor: usize,
    ) -> Result<(Vec<Vec<E>>, Vec<BatchMerkleProof<H>>), DeserializationError>
    where
        E: FieldElement,
        H: ElementHasher<BaseField = E::BaseField>,
    {
        assert!(domain_size.is_power_of_two(), "domain size must be a power of two");
        assert!(folding_factor.is_power_of_two(), "folding factor must be a power of two");
        assert!(folding_factor > 1, "folding factor must be greater than 1");

        let mut layer_proofs = Vec::new();
        let mut layer_queries = Vec::new();

        // parse all layers
        for (i, layer) in self.layers.into_iter().enumerate() {
            domain_size /= folding_factor;
            let (qv, mp) = layer.parse(domain_size, folding_factor).map_err(|err| {
                DeserializationError::InvalidValue(format!("failed to parse FRI layer {i}: {err}"))
            })?;
            layer_proofs.push(mp);
            layer_queries.push(qv);
        }

        Ok((layer_queries, layer_proofs))
    }

    /// Returns a vector of remainder values (last FRI layer) parsed from this proof.
    ///
    /// # Errors
    /// Returns an error if:
    /// * The number of remainder values implied by a combination of `E` type parameter and
    ///   the number of remainder bytes in this proof is not a power of two.
    /// * Any of the remainder values could not be parsed correctly.
    /// * Not all bytes have been consumed while parsing remainder values.
    pub fn parse_remainder<E: FieldElement>(&self) -> Result<Vec<E>, DeserializationError> {
        let num_elements = self.num_remainder_elements::<E>();
        if !num_elements.is_power_of_two() {
            return Err(DeserializationError::InvalidValue(format!(
                "number of remainder values must be a power of two, but {num_elements} was implied"
            )));
        }
        let mut reader = SliceReader::new(&self.remainder);
        let remainder = E::read_batch_from(&mut reader, num_elements).map_err(|err| {
            DeserializationError::InvalidValue(format!("failed to parse FRI remainder: {err}"))
        })?;
        if reader.has_more_bytes() {
            return Err(DeserializationError::UnconsumedBytes);
        }
        Ok(remainder)
    }
}

// SERIALIZATION / DESERIALIZATION
// ------------------------------------------------------------------------------------------------

impl Serializable for FriProof {
    /// Serializes `self` and writes the resulting bytes into the `target` writer.
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        // write layers
        target.write_u8(self.layers.len() as u8);
        for layer in self.layers.iter() {
            layer.write_into(target);
        }

        // write remainder
        target.write_u16(self.remainder.len() as u16);
        target.write_bytes(&self.remainder);

        // write number of partitions
        target.write_u8(self.num_partitions);
    }
}

impl Deserializable for FriProof {
    /// Reads a FRI proof from the specified `source` and returns the result.
    ///
    /// # Errors
    /// Returns an error if a valid proof could not be read from the source.
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        // read layers
        let num_layers = source.read_u8()? as usize;
        let layers = FriProofLayer::read_batch_from(source, num_layers)?;

        // read remainder
        let num_remainder_bytes = source.read_u16()? as usize;
        let remainder = source.read_vec(num_remainder_bytes)?;

        // read number of partitions
        let num_partitions = source.read_u8()?;

        Ok(FriProof {
            layers,
            remainder,
            num_partitions,
        })
    }
}

// FRI PROOF LAYER
// ================================================================================================

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FriProofLayer {
    values: Vec<u8>,
    paths: Vec<u8>,
}

impl FriProofLayer {
    // CONSTRUCTOR
    // --------------------------------------------------------------------------------------------
    /// Creates a new proof layer from the specified query values and the corresponding Merkle
    /// paths aggregated into a single batch Merkle proof.
    ///
    /// # Panics
    /// Panics if `query_values` is an empty slice.
    pub(crate) fn new<H: Hasher, E: FieldElement, const N: usize>(
        query_values: Vec<[E; N]>,
        merkle_proof: BatchMerkleProof<H>,
    ) -> Self {
        assert!(!query_values.is_empty(), "query values cannot be empty");

        // TODO: add debug check that values actually hash into the leaf nodes of the batch proof

        // concatenate all query values and all internal Merkle proof nodes into vectors of bytes;
        // we care about internal nodes only because leaf nodes can be reconstructed from hashes
        // of query values
        FriProofLayer {
            values: query_values.to_bytes(),
            paths: merkle_proof.serialize_nodes(),
        }
    }

    // PUBLIC ACCESSORS
    // --------------------------------------------------------------------------------------------

    /// Returns the size of this proof layer in bytes.
    pub fn size(&self) -> usize {
        // +4 for length of values, +4 for length of paths
        self.values.len() + 4 + self.paths.len() + 4
    }

    // PARSING
    // --------------------------------------------------------------------------------------------
    /// Decomposes this layer into a combination of query values and corresponding Merkle
    /// authentication paths (grouped together into a single batch Merkle proof).
    ///
    /// # Errors
    /// Returns an error if:
    /// * This layer does not contain at least one query.
    /// * Parsing of any of the query values or the corresponding Merkle paths fails.
    /// * Not all bytes have been consumed while parsing this layer.
    pub fn parse<H, E>(
        self,
        domain_size: usize,
        folding_factor: usize,
    ) -> Result<(Vec<E>, BatchMerkleProof<H>), DeserializationError>
    where
        E: FieldElement,
        H: ElementHasher<BaseField = E::BaseField>,
    {
        // make sure the number of value bytes can be parsed into a whole number of queries
        let num_query_bytes = E::ELEMENT_BYTES * folding_factor;
        if self.values.len() % num_query_bytes != 0 {
            return Err(DeserializationError::InvalidValue(format!(
                "number of value bytes ({}) does not divide into whole number of queries",
                self.values.len(),
            )));
        }

        let num_queries = self.values.len() / num_query_bytes;
        if num_queries == 0 {
            return Err(DeserializationError::InvalidValue(
                "a FRI layer must contain at least one query".to_string(),
            ));
        }
        let mut hashed_queries = vec![H::Digest::default(); num_queries];
        let mut query_values = Vec::with_capacity(num_queries * folding_factor);

        // read bytes corresponding to each query, convert them into field elements,
        // and also hash them to build leaf nodes of the batch Merkle proof
        let mut reader = SliceReader::new(&self.values);
        for query_hash in hashed_queries.iter_mut() {
            let mut qe = E::read_batch_from(&mut reader, folding_factor)?;
            *query_hash = H::hash_elements(&qe);
            query_values.append(&mut qe);
        }
        if reader.has_more_bytes() {
            return Err(DeserializationError::UnconsumedBytes);
        }

        // build batch Merkle proof
        let mut reader = SliceReader::new(&self.paths);
        let tree_depth = domain_size.ilog2() as u8;
        let merkle_proof = BatchMerkleProof::deserialize(&mut reader, hashed_queries, tree_depth)?;
        if reader.has_more_bytes() {
            return Err(DeserializationError::UnconsumedBytes);
        }

        Ok((query_values, merkle_proof))
    }
}

// SERIALIZATION / DESERIALIZATION
// ------------------------------------------------------------------------------------------------

impl Serializable for FriProofLayer {
    /// Serializes this proof layer and writes the resulting bytes to the specified `target`.
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        // write value bytes
        target.write_u32(self.values.len() as u32);
        target.write_bytes(&self.values);

        // write path bytes
        target.write_u32(self.paths.len() as u32);
        target.write_bytes(&self.paths);
    }
}

impl Deserializable for FriProofLayer {
    /// Reads a single proof layer form the `source` and returns it.
    ///
    /// # Errors
    /// Returns an error if a valid layer could not be read from the specified source.
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        // read values
        let num_value_bytes = source.read_u32()?;
        if num_value_bytes == 0 {
            return Err(DeserializationError::InvalidValue(
                "a FRI proof layer must contain at least one queried evaluation".to_string(),
            ));
        }
        let values = source.read_vec(num_value_bytes as usize)?;

        // read paths
        let num_paths_bytes = source.read_u32()?;
        let paths = source.read_vec(num_paths_bytes as usize)?;

        Ok(FriProofLayer { values, paths })
    }
}
