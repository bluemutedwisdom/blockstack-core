use vm::representations::{SymbolicExpressionType, SymbolicExpression, ClarityName};
use vm::representations::SymbolicExpressionType::{AtomValue, Atom, List, LiteralValue};
use vm::types::{TypeSignature, TupleTypeSignature, Value, PrincipalData, parse_name_type_pairs};
use vm::functions::NativeFunctions;
use vm::functions::define::DefineFunctions;
use vm::functions::tuples;
use vm::functions::tuples::TupleDefinitionType::{Implicit, Explicit};
use vm::analysis::types::{ContractAnalysis, AnalysisPass};

use vm::variables::NativeVariables;
use std::collections::HashMap;

use super::AnalysisDatabase;
pub use super::errors::{CheckResult, CheckError, CheckErrors, check_argument_count, check_arguments_at_least};

#[cfg(test)]
mod tests;

pub struct ReadOnlyChecker <'a, 'b> {
    db: &'a mut AnalysisDatabase<'b>,
    defined_functions: HashMap<ClarityName, bool>
}

impl <'a, 'b> AnalysisPass for ReadOnlyChecker <'a, 'b> {

    fn run_pass(contract_analysis: &mut ContractAnalysis, analysis_db: &mut AnalysisDatabase) -> CheckResult<()> {
        let mut command = ReadOnlyChecker::new(analysis_db);
        command.run(contract_analysis)?;
        Ok(())
    }
}

impl <'a, 'b> ReadOnlyChecker <'a, 'b> {
    
    fn new(db: &'a mut AnalysisDatabase<'b>) -> ReadOnlyChecker<'a, 'b> {
        Self { 
            db, 
            defined_functions: HashMap::new() 
        }
    }

    pub fn run(& mut self, contract_analysis: &mut ContractAnalysis) -> CheckResult<()> {

        for exp in contract_analysis.expressions_iter() {
            let mut result = self.check_reads_only_valid(&exp);
            if let Err(ref mut error) = result {
                if !error.has_expression() {
                    error.set_expression(&exp);
                }
            }
            result?
        }

        Ok(())
    }

    fn check_define_function(&mut self, args: &[SymbolicExpression]) -> CheckResult<(ClarityName, bool)> {
        check_argument_count(2, args)?;

        let signature = args[0].match_list()
            .ok_or(CheckErrors::DefineFunctionBadSignature)?;
        let body = &args[1];

        let function_name = signature.get(0)
            .ok_or(CheckErrors::DefineFunctionBadSignature)?
            .match_atom().ok_or(CheckErrors::BadFunctionName)?;

        let is_read_only = self.is_read_only(body)?;

        Ok((function_name.clone(), is_read_only))
    }

    fn check_reads_only_valid(&mut self, expr: &SymbolicExpression) -> CheckResult<()> {
        use vm::functions::define::DefineFunctions::*;
        if let Some((define_type, args)) = DefineFunctions::try_parse(expr) {
            match define_type {
                Constant | Map | PersistedVariable | FungibleToken | NonFungibleToken => {
                    // None of these define types ever need to be checked for their
                    //  read-onliness, since they're never invoked outside of contract initialization.
                    Ok(())
                },
                PrivateFunction => {
                    let (f_name, is_read_only) = self.check_define_function(args)?;
                    self.defined_functions.insert(f_name, is_read_only);
                    Ok(())
                },
                PublicFunction => {
                    let (f_name, is_read_only) = self.check_define_function(args)?;
                    self.defined_functions.insert(f_name, is_read_only);
                    Ok(())
                },
                ReadOnlyFunction => {
                    let (f_name, is_read_only) = self.check_define_function(args)?;
                    if !is_read_only {
                        Err(CheckErrors::WriteAttemptedInReadOnly.into())
                    } else {
                        self.defined_functions.insert(f_name, is_read_only);
                        Ok(())
                    }
                },
            }
        } else {
            Ok(())
        }
    }

    fn are_all_read_only(&mut self, initial: bool, expressions: &[SymbolicExpression]) -> CheckResult<bool> {
        expressions.iter()
            .fold(Ok(initial),
                  |acc, argument| {
                      Ok(acc? && self.is_read_only(&argument)?) })
    }

    fn is_implicit_tuple_definition_read_only(&mut self, tuples: &[SymbolicExpression]) -> CheckResult<bool> {
        for tuple_expr in tuples.iter() {
            let pair = tuple_expr.match_list()
                .ok_or(CheckErrors::TupleExpectsPairs)?;
            if pair.len() != 2 {
                return Err(CheckErrors::TupleExpectsPairs.into())
            }

            if !self.is_read_only(&pair[1])? {
                return Ok(false)
            }
        }
        Ok(true)
    }

    fn try_native_function_check(&mut self, function: &str, args: &[SymbolicExpression]) -> Option<CheckResult<bool>> {
        if let Some(ref function) = NativeFunctions::lookup_by_name(function) {
            Some(self.handle_native_function(function, args))
        } else {
            None
        }
    }

    fn handle_native_function(&mut self, function: &NativeFunctions, args: &[SymbolicExpression]) -> CheckResult<bool> {
        use vm::functions::NativeFunctions::*;

        match function {
            Add | Subtract | Divide | Multiply | CmpGeq | CmpLeq | CmpLess | CmpGreater |
            Modulo | Power | BitwiseXOR | And | Or | Not | Hash160 | Sha256 | Keccak256 | Equals | If |
            Sha512 | Sha512Trunc256 |
            ConsSome | ConsOkay | ConsError | DefaultTo | Expects | ExpectsErr | IsOkay | IsNone |
            ToUInt | ToInt |
            ListCons | GetBlockInfo | TupleGet | Print | AsContract | Begin | FetchVar | GetTokenBalance | GetAssetOwner => {
                self.are_all_read_only(true, args)
            },
            FetchEntry => {                
                let res = match tuples::get_definition_type_of_tuple_argument(&args[1]) {
                    Implicit(ref tuple_expr) => {
                        self.is_implicit_tuple_definition_read_only(tuple_expr)
                    },
                    Explicit => {
                        self.are_all_read_only(true, args)
                    }
                };
                res
            },
            FetchContractEntry => {                
                let res = match tuples::get_definition_type_of_tuple_argument(&args[2]) {
                    Implicit(ref tuple_expr) => {
                        self.is_implicit_tuple_definition_read_only(tuple_expr)
                    },
                    Explicit => {
                        self.are_all_read_only(true, args)
                    }
                };
                res
            },
            SetEntry | DeleteEntry | InsertEntry | SetVar | MintAsset | MintToken | TransferAsset | TransferToken => {
                Ok(false)
            },
            Let => {
                check_arguments_at_least(2, args)?;
    
                let binding_list = args[0].match_list()
                    .ok_or(CheckErrors::BadLetSyntax)?;

                for pair in binding_list.iter() {
                    let pair_expression = pair.match_list()
                        .ok_or(CheckErrors::BadSyntaxBinding)?;
                    if pair_expression.len() != 2 {
                        return Err(CheckErrors::BadSyntaxBinding.into())
                    }

                    if !self.is_read_only(&pair_expression[1])? {
                        return Ok(false)
                    }
                }

                self.are_all_read_only(true, &args[1..args.len()])
            },
            Map | Filter => {
                check_argument_count(2, args)?;
    
                // note -- we do _not_ check here to make sure we're not mapping on
                //      a special function. that check is performed by the type checker.
                //   we're pretty directly violating type checks in this recursive step:
                //   we're asking the read only checker to check whether a function application
                //     of the _mapping function_ onto the rest of the supplied arguments would be
                //     read-only or not.
                self.is_function_application_read_only(args)
            },
            Fold => {
                check_argument_count(3, args)?;
    
                // note -- we do _not_ check here to make sure we're not folding on
                //      a special function. that check is performed by the type checker.
                //   we're pretty directly violating type checks in this recursive step:
                //   we're asking the read only checker to check whether a function application
                //     of the _folding function_ onto the rest of the supplied arguments would be
                //     read-only or not.
                self.is_function_application_read_only(args)
            },
            TupleCons => {
                for pair in args.iter() {
                    let pair_expression = pair.match_list()
                        .ok_or(CheckErrors::TupleExpectsPairs)?;
                    if pair_expression.len() != 2 {
                        return Err(CheckErrors::TupleExpectsPairs.into())
                    }

                    if !self.is_read_only(&pair_expression[1])? {
                        return Ok(false)
                    }
                }
                Ok(true)
            },
            ContractCall => {
                check_arguments_at_least(2, args)?;
                let contract_identifier = match args[0].expr {
                    SymbolicExpressionType::LiteralValue(Value::Principal(PrincipalData::Contract(ref contract_identifier))) => contract_identifier,
                    _ => return Err(CheckError::new(CheckErrors::ContractCallExpectName))
                };

                let function_name = args[1].match_atom()
                    .ok_or(CheckErrors::ContractCallExpectName)?;

                let is_function_read_only = self.db.get_read_only_function_type(&contract_identifier, function_name)?.is_some();
                self.are_all_read_only(is_function_read_only, &args[2..])
            }
        }
    }

    fn is_function_application_read_only(&mut self, expression: &[SymbolicExpression]) -> CheckResult<bool> {
        let (function_name, args) = expression.split_first()
            .ok_or(CheckErrors::NonFunctionApplication)?;

        let function_name = function_name.match_atom()
            .ok_or(CheckErrors::NonFunctionApplication)?;

        if let Some(result) = self.try_native_function_check(function_name, args) {
            result
        } else {
            let is_function_read_only = self.defined_functions.get(function_name)
                .ok_or(CheckErrors::UnknownFunction(function_name.to_string()))?
                .clone();
            self.are_all_read_only(is_function_read_only, args)
        }
    }


    fn is_read_only(&mut self, expr: &SymbolicExpression) -> CheckResult<bool> {
        match expr.expr {
            AtomValue(_) | LiteralValue(_) => {
                Ok(true)
            },
            Atom(_) => {
                Ok(true)
            },
            List(ref expression) => {
                self.is_function_application_read_only(expression)
            }
        }
    }
}
