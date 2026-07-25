use flpdf::{Matrix, Rectangle};

#[test]
fn default_constructor_and_raw_matrix_round_trip_match_qpdf() {
    let identity = Matrix::default();
    assert_eq!(identity.get_as_matrix(), [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    let raw_matrix = [3.0, 1.0, 4.0, 1.0, 5.0, 9.26535];
    let round_tripped_matrix: [f64; 6] = Matrix::from(raw_matrix).into();
    assert_eq!(round_tripped_matrix, raw_matrix);

    let raw_rectangle = [2.0, 7.0, 1.0, 8.0];
    let round_tripped_rectangle: [f64; 4] = Rectangle::from(raw_rectangle).into();
    assert_eq!(round_tripped_rectangle, raw_rectangle);
}

#[test]
fn concat_scale_translate_and_transform_follow_qpdf_order() {
    let mut matrix = Matrix::default();
    matrix.translate(10.0, 20.0);
    matrix.scale(1.5, 2.0);
    assert_eq!(matrix.transform(10.0, 100.0), (25.0, 220.0));

    matrix.translate(30.0, 40.0);
    matrix.concat(Matrix::new(1.0, 2.0, 3.0, 4.0, 5.0, 6.0));

    assert_eq!(matrix.get_as_matrix(), [1.5, 4.0, 4.5, 8.0, 62.5, 112.0]);
    assert_eq!(matrix.transform(240.0, 480.0), (2582.5, 4912.0));
}

#[test]
fn rotatex90_handles_quarter_turns_and_ignores_other_angles() {
    let base = Matrix::new(1.5, 4.0, 4.5, 8.0, 62.5, 112.0);

    let mut matrix = base;
    matrix.rotatex90(90);
    assert_eq!(matrix.get_as_matrix(), [4.5, 8.0, -1.5, -4.0, 62.5, 112.0]);
    matrix.rotatex90(180);
    assert_eq!(matrix.get_as_matrix(), [-4.5, -8.0, 1.5, 4.0, 62.5, 112.0]);
    matrix.rotatex90(270);
    assert_eq!(
        matrix.get_as_matrix(),
        [-1.5, -4.0, -4.5, -8.0, 62.5, 112.0]
    );
    matrix.rotatex90(180);
    matrix.rotatex90(12345);
    assert_eq!(matrix, base);
}

#[test]
fn transform_rectangle_tightly_bounds_all_four_transformed_corners() {
    let mut matrix = Matrix::default();
    matrix.rotatex90(90);
    matrix.translate(200.0, -100.0);

    assert_eq!(
        matrix.transform_rectangle(Rectangle::new(10.0, 20.0, 30.0, 50.0)),
        Rectangle::new(50.0, 210.0, 80.0, 230.0)
    );
}

#[test]
fn unparse_matches_qpdf_rounding_and_trimming() {
    assert_eq!(Matrix::default().unparse(), "1 0 0 1 0 0");
    assert_eq!(
        Matrix::from([0.000_004, -0.0, 0.000_01, -0.000_01, 9.26535, 0.0]).unparse(),
        "0 0 0.00001 -0.00001 9.26535 0"
    );
}
